// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_property.c`.
//!
//! Property creation (plain, enum, bitmask, range, object, bool), enum value
//! registration, property ioctl (`GETPROPERTY`), refcounted property blobs
//! (create, lookup, replace, get/put, `GETBLOB`/`CREATEBLOB`/`DESTROYBLOB`),
//! and the atomic-free `drm_property_change_valid_get()` / `_put()` pair.

use core::{
    ffi::{c_char, c_void},
    mem::{offset_of, size_of},
};

use drm_kapi::{
    assert::warn_on,
    config::{EFAULT, EINVAL, ENOENT, ENOMEM, EOPNOTSUPP, EPERM, GFP_KERNEL},
    list::{init_list_head, list_add_tail, list_del, list_del_init},
    memory::{kfree, kvfree, kzalloc},
    mm_native::{copy_from_user_c, copy_to_user_c},
    types::{Kref, ListHead},
};

use crate::{
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::{
        __drm_mode_object_add, __drm_mode_object_find, DRIVER_MODESET, drm_core_check_feature,
        drm_mode_object_add, drm_mode_object_find, drm_mode_object_get, drm_mode_object_put,
        drm_mode_object_unregister, drm_object_property_set_value, mutex_lock, mutex_unlock,
    },
    drm_property_types::{DrmPropEnumList, DrmProperty, DrmPropertyBlob, DrmPropertyEnum},
    uapi_drm_consts::{
        DRM_MODE_OBJECT_BLOB, DRM_MODE_OBJECT_PROPERTY, DRM_MODE_PROP_ATOMIC,
        DRM_MODE_PROP_BITMASK, DRM_MODE_PROP_BLOB, DRM_MODE_PROP_ENUM, DRM_MODE_PROP_EXTENDED_TYPE,
        DRM_MODE_PROP_IMMUTABLE, DRM_MODE_PROP_LEGACY_TYPE, DRM_MODE_PROP_OBJECT,
        DRM_MODE_PROP_RANGE, DRM_MODE_PROP_SIGNED_RANGE,
    },
    uapi_drm_mode_types::{
        DrmModeCreateBlob, DrmModeDestroyBlob, DrmModeGetBlob, DrmModeGetProperty,
        DrmModePropertyEnum,
    },
};

/// `DRM_PROP_NAME_LEN` (include/drm/drm_property.h).
pub const DRM_PROP_NAME_LEN: usize = 32;

/// Size of `struct drm_mode_property_enum` in user space (uapi/drm/drm_mode.h).
const USER_PROPERTY_ENUM_SIZE: usize = size_of::<DrmModePropertyEnum>();

// upstream: include/drm/drm_property.h drm_property_type_is()
/// # Safety
///
/// `property` must be non-null, properly aligned, and point to an initialized `DrmProperty`
/// whose `flags` field is valid for reading without concurrent unsynchronized writes.
pub(crate) unsafe fn drm_property_type_is(property: *mut DrmProperty, r#type: u32) -> bool {
    // SAFETY: `property` is non-null and points to an initialized `DrmProperty` per the function's safety contract.
    let flags = unsafe { (*property).flags };
    // instanceof for props.. handles extended type vs original types:
    if flags & DRM_MODE_PROP_EXTENDED_TYPE != 0 {
        return (flags & DRM_MODE_PROP_EXTENDED_TYPE) == r#type;
    }
    flags & r#type != 0
}

// upstream: include/drm/drm_property.h obj_to_property()
/// # Safety
///
/// `obj` must be a non-null pointer to the embedded `base` field of an allocated `DrmProperty`
/// struct so that subtracting `offset_of!(DrmProperty, base)` yields a valid pointer within the same allocation.
pub(crate) unsafe fn obj_to_property(obj: *mut DrmModeObject) -> *mut DrmProperty {
    // SAFETY: per the function's safety contract, `obj` points to the `base` member of a live `DrmProperty`, so stepping back by its offset stays within that object.
    unsafe {
        obj.cast::<u8>()
            .sub(offset_of!(DrmProperty, base))
            .cast::<DrmProperty>()
    }
}

// upstream: include/drm/drm_property.h obj_to_blob()
/// # Safety
///
/// `obj` must be a non-null pointer to the embedded `base` field of an allocated `DrmPropertyBlob`
/// struct so that subtracting `offset_of!(DrmPropertyBlob, base)` yields a valid pointer within the same allocation.
pub(crate) unsafe fn obj_to_blob(obj: *mut DrmModeObject) -> *mut DrmPropertyBlob {
    // SAFETY: per the function's safety contract, `obj` points to the `base` member of a live `DrmPropertyBlob`, so stepping back by its offset stays within that object.
    unsafe {
        obj.cast::<u8>()
            .sub(offset_of!(DrmPropertyBlob, base))
            .cast::<DrmPropertyBlob>()
    }
}

// upstream: include/drm/drm_property.h drm_property_find()
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice` whose mode-config registry is accessible.
/// - If `file_priv` is non-null, it must point to an initialized, live `DrmFile` structure.
/// - Caller must preserve appropriate synchronization (e.g. mode-config lock or registry lifetime) for the lookup.
pub(crate) unsafe fn drm_property_find(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    id: u32,
) -> *mut DrmProperty {
    // SAFETY: `dev` is live and `file_priv` is null or an open `DrmFile` per the function's safety contract.
    let mo = unsafe { drm_mode_object_find(dev, file_priv, id, DRM_MODE_OBJECT_PROPERTY) };
    if mo.is_null() {
        core::ptr::null_mut()
    } else {
        // SAFETY: `mo` is non-null here and comes from the property-typed lookup above, so it is the `base` member of a live `DrmProperty`.
        unsafe { obj_to_property(mo) }
    }
}

/// Logs a DRM atomic debug message (`drm_dbg_atomic()`), via the kapi logger.
macro_rules! drm_dbg_atomic {
    ($device:expr, $format:expr $(, $argument:expr)* $(,)?) => {
        drm_kapi::drm_dbg!($device, $format $(, $argument)*)
    };
}

/// `strlen()` on a NUL-terminated C string.
///
/// # Safety
///
/// `s` must be non-null, properly aligned, and point to a contiguous sequence of readable
/// bytes terminated by a NUL byte (`0`) that is not concurrently mutated during this call.
unsafe fn c_strlen(s: *const c_char) -> usize {
    let mut len = 0usize;
    // SAFETY: `s` is a NUL-terminated string per the function's safety contract; the loop stops at the NUL, so `s.add(len)` stays within the string.
    while unsafe { *s.add(len) != 0 } {
        len += 1;
    }
    len
}

/// `strscpy_pad()`: copies at most `dst.len() - 1` bytes and zero-fills the rest.
///
/// # Safety
///
/// - `dst` must be non-null and point to a writable memory buffer of at least `size` bytes.
/// - `src` must be non-null and point to a valid NUL-terminated C string in readable memory.
/// - The memory ranges of `dst` and `src` must not overlap.
unsafe fn strscpy_pad(dst: *mut c_char, src: *const c_char, size: usize) {
    let mut i = 0usize;
    while i + 1 < size {
        // SAFETY: every byte before index `i` was non-NUL, so `src[i]` lies within the NUL-terminated string `src` per the function's safety contract.
        let c = unsafe { *src.add(i) };
        // SAFETY: the loop condition gives `i < size`, and `dst` is writable for `size` bytes per the function's safety contract.
        unsafe { *dst.add(i) = c };
        if c == 0 {
            break;
        }
        i += 1;
    }
    while i < size {
        // SAFETY: the loop condition gives `i < size`, and `dst` is writable for `size` bytes per the function's safety contract.
        unsafe { *dst.add(i) = 0 };
        i += 1;
    }
}

/// `put_user()` for a single scalar: returns 0 or -EFAULT.
// upstream: include/linux/uaccess.h put_user()
/// # Safety
///
/// `dst` must point to user-space memory destination capable of receiving `size_of::<T>()` bytes
/// via `copy_to_user_c`.
unsafe fn put_user<T: Copy>(dst: *mut T, value: T) -> i32 {
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

/// `container_of(kref, struct drm_property_blob, base.refcount)`.
///
/// # Safety
///
/// `kref` must be a non-null pointer to the `base.refcount` field of an allocated `DrmPropertyBlob`
/// struct so that subtracting the offset of `base.refcount` yields a valid pointer within the same allocation.
unsafe fn blob_from_kref(kref: *mut Kref) -> *mut DrmPropertyBlob {
    // SAFETY: per the function's safety contract, `kref` is the `base.refcount` member of a live `DrmPropertyBlob`, so stepping back by these offsets stays within that object.
    unsafe {
        kref.cast::<u8>()
            .sub(offset_of!(DrmModeObject, refcount))
            .sub(offset_of!(DrmPropertyBlob, base))
            .cast::<DrmPropertyBlob>()
    }
}

// upstream: drm_property.c drm_property_flags_valid()
fn drm_property_flags_valid(flags: u32) -> bool {
    let legacy_type = flags & DRM_MODE_PROP_LEGACY_TYPE;
    let ext_type = flags & DRM_MODE_PROP_EXTENDED_TYPE;

    // Reject undefined/deprecated flags
    if flags
        & !(DRM_MODE_PROP_LEGACY_TYPE
            | DRM_MODE_PROP_EXTENDED_TYPE
            | DRM_MODE_PROP_IMMUTABLE
            | DRM_MODE_PROP_ATOMIC)
        != 0
    {
        return false;
    }

    // We want either a legacy type or an extended type, but not both
    if legacy_type == 0 && ext_type == 0 || legacy_type != 0 && ext_type != 0 {
        return false;
    }

    // Only one legacy type at a time please
    if legacy_type != 0 && !legacy_type.is_power_of_two() {
        return false;
    }

    true
}

// upstream: drm_property.c drm_property_create()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to a live, initialized `DrmDevice`.
/// - `name` must be non-null and point to a readable NUL-terminated C string representing the property name.
/// - `num_values` must be non-negative.
/// - Caller must hold `dev->mode_config.mutex` (or call during early device initialization before concurrent registration).
pub unsafe extern "C" fn drm_property_create(
    dev: *mut DrmDevice,
    flags: u32,
    name: *const c_char,
    num_values: i32,
) -> *mut DrmProperty {
    // SAFETY: `dev` is a live `DrmDevice`; `name` is checked for NUL-termination and length < `DRM_PROP_NAME_LEN`; `property` and `values` allocations are checked for null and fully initialized before registering with `drm_mode_object_add_property` and linking into `dev->mode_config.property_list`.
    unsafe {
        if warn_on(!drm_property_flags_valid(flags)) {
            return core::ptr::null_mut();
        }
        if warn_on(c_strlen(name) >= DRM_PROP_NAME_LEN) {
            return core::ptr::null_mut();
        }

        let property = drm_kapi::memory::kzalloc_obj::<DrmProperty>();
        if property.is_null() {
            return core::ptr::null_mut();
        }

        (*property).dev = dev;

        if num_values != 0 {
            (*property).values =
                kzalloc(num_values as usize * size_of::<u64>(), GFP_KERNEL).cast::<u64>();
            if (*property).values.is_null() {
                return fail_property(property);
            }
        }

        let ret = drm_mode_object_add_property(dev, property);
        if ret != 0 {
            return fail_property(property);
        }

        (*property).flags = flags;
        (*property).num_values = num_values as u32;
        init_list_head(core::ptr::addr_of_mut!((*property).enum_list));

        strscpy_pad((*property).name.as_mut_ptr(), name, DRM_PROP_NAME_LEN);

        list_add_tail(
            core::ptr::addr_of_mut!((*property).head),
            core::ptr::addr_of_mut!((*dev).mode_config.property_list),
        );

        property
    }
}

/// `fail:` label of `drm_property_create()`.
///
/// # Safety
///
/// `property` must be non-null and point to a heap-allocated `DrmProperty` being torn down upon creation failure;
/// its `values` pointer must be either null or allocated via `kzalloc`. Neither pointer has been registered or freed.
unsafe fn fail_property(property: *mut DrmProperty) -> *mut DrmProperty {
    // SAFETY: `property` and `(*property).values` are unaliased heap allocations created by `kzalloc` in `drm_property_create` and reclaimed via `kfree`.
    unsafe {
        kfree((*property).values);
        kfree(property);
        core::ptr::null_mut()
    }
}

/// `drm_mode_object_add(dev, &property->base, DRM_MODE_OBJECT_PROPERTY)`.
///
/// # Safety
///
/// `dev` must point to a live `DrmDevice`, and `property` must point to an allocated `DrmProperty`
/// whose `base` field is ready to be initialized as a `DRM_MODE_OBJECT_PROPERTY`.
unsafe fn drm_mode_object_add_property(dev: *mut DrmDevice, property: *mut DrmProperty) -> i32 {
    // SAFETY: `dev` is a live `DrmDevice` and `property` is an allocated `DrmProperty` whose embedded `base` field is accessible via `addr_of_mut!` for mode object registration.
    unsafe {
        drm_mode_object_add(
            dev,
            core::ptr::addr_of_mut!((*property).base),
            DRM_MODE_OBJECT_PROPERTY,
        )
    }
}

// upstream: drm_property.c drm_property_create_enum()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - `name` must be non-null and point to a readable NUL-terminated C string.
/// - If `num_values > 0`, `props` must point to a readable array of at least `num_values` initialized `DrmPropEnumList` elements, each containing a valid NUL-terminated `name` C string.
/// - `num_values` must be non-negative.
/// - Caller must hold `dev->mode_config.mutex` (or call during early device registration).
pub unsafe extern "C" fn drm_property_create_enum(
    dev: *mut DrmDevice,
    mut flags: u32,
    name: *const c_char,
    props: *const DrmPropEnumList,
    num_values: i32,
) -> *mut DrmProperty {
    // SAFETY: `dev` and `name` are forwarded to `drm_property_create`; `props.add(i)` accesses elements `0..num_values` within the caller-guaranteed array bounds of `props`, and any failure unregisters and destroys the partially constructed property via `drm_property_destroy`.
    unsafe {
        flags |= DRM_MODE_PROP_ENUM;

        let property = drm_property_create(dev, flags, name, num_values);
        if property.is_null() {
            return core::ptr::null_mut();
        }

        for i in 0..num_values as usize {
            let ret = drm_property_add_enum(
                property,
                (*props.add(i)).r#type as u64,
                (*props.add(i)).name,
            );
            if ret != 0 {
                drm_property_destroy(dev, property);
                return core::ptr::null_mut();
            }
        }

        property
    }
}

// upstream: drm_property.c drm_property_create_bitmask()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - `name` must be non-null and point to a readable NUL-terminated C string.
/// - If `num_props > 0`, `props` must point to a readable array of at least `num_props` initialized `DrmPropEnumList` elements, each containing a valid NUL-terminated `name` C string.
/// - `num_props` must be non-negative.
/// - Caller must hold `dev->mode_config.mutex` (or call during early device registration).
pub unsafe extern "C" fn drm_property_create_bitmask(
    dev: *mut DrmDevice,
    mut flags: u32,
    name: *const c_char,
    props: *const DrmPropEnumList,
    num_props: i32,
    supported_bits: u64,
) -> *mut DrmProperty {
    // SAFETY: `dev` and `name` are forwarded to `drm_property_create` with `num_values` derived from `supported_bits.count_ones()`; `props.add(i)` reads elements within caller-guaranteed bounds `0..num_props`, and failures are rolled back via `drm_property_destroy`.
    unsafe {
        let num_values = supported_bits.count_ones() as i32;

        flags |= DRM_MODE_PROP_BITMASK;

        let property = drm_property_create(dev, flags, name, num_values);
        if property.is_null() {
            return core::ptr::null_mut();
        }

        for i in 0..num_props as usize {
            if supported_bits & (1u64 << (*props.add(i)).r#type) == 0 {
                continue;
            }

            let ret = drm_property_add_enum(
                property,
                (*props.add(i)).r#type as u64,
                (*props.add(i)).name,
            );
            if ret != 0 {
                drm_property_destroy(dev, property);
                return core::ptr::null_mut();
            }
        }

        property
    }
}

// upstream: drm_property.c property_create_range()
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `name` must point to a valid NUL-terminated C string.
/// - Caller must hold `dev->mode_config.mutex` if required.
unsafe fn property_create_range(
    dev: *mut DrmDevice,
    flags: u32,
    name: *const c_char,
    min: u64,
    max: u64,
) -> *mut DrmProperty {
    // SAFETY: `drm_property_create(dev, flags, name, 2)` allocates a 2-element values array; if successful, offsets 0 and 1 are within bounds for storing `min` and `max`.
    unsafe {
        let property = drm_property_create(dev, flags, name, 2);
        if property.is_null() {
            return core::ptr::null_mut();
        }

        *(*property).values.add(0) = min;
        *(*property).values.add(1) = max;

        property
    }
}

// upstream: drm_property.c drm_property_create_range()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `name` must point to a valid NUL-terminated C string.
/// - Caller must hold `dev->mode_config.mutex` (or call during early device registration).
pub unsafe extern "C" fn drm_property_create_range(
    dev: *mut DrmDevice,
    flags: u32,
    name: *const c_char,
    min: u64,
    max: u64,
) -> *mut DrmProperty {
    // SAFETY: `dev` and `name` satisfy preconditions of `property_create_range` per function contract with `DRM_MODE_PROP_RANGE` flag set.
    unsafe { property_create_range(dev, DRM_MODE_PROP_RANGE | flags, name, min, max) }
}

// upstream: drm_property.c drm_property_create_signed_range()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `name` must point to a valid NUL-terminated C string.
/// - Caller must hold `dev->mode_config.mutex` (or call during early device registration).
pub unsafe extern "C" fn drm_property_create_signed_range(
    dev: *mut DrmDevice,
    flags: u32,
    name: *const c_char,
    min: i64,
    max: i64,
) -> *mut DrmProperty {
    // SAFETY: `dev` and `name` satisfy preconditions of `property_create_range` per function contract; `min` and `max` bit-casts to `u64` are stored for `DRM_MODE_PROP_SIGNED_RANGE`.
    unsafe {
        property_create_range(
            dev,
            DRM_MODE_PROP_SIGNED_RANGE | flags,
            name,
            min as u64,
            max as u64,
        )
    }
}

// upstream: drm_property.c drm_property_create_object()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `name` must point to a valid NUL-terminated C string.
/// - Caller must hold `dev->mode_config.mutex` (or call during early device registration).
pub unsafe extern "C" fn drm_property_create_object(
    dev: *mut DrmDevice,
    mut flags: u32,
    name: *const c_char,
    r#type: u32,
) -> *mut DrmProperty {
    // SAFETY: `drm_property_create(dev, flags, name, 1)` allocates a 1-element values array; if successful, index 0 is valid for storing `r#type as u64`.
    unsafe {
        flags |= DRM_MODE_PROP_OBJECT;

        if warn_on(flags & DRM_MODE_PROP_ATOMIC == 0) {
            return core::ptr::null_mut();
        }

        let property = drm_property_create(dev, flags, name, 1);
        if property.is_null() {
            return core::ptr::null_mut();
        }

        *(*property).values.add(0) = r#type as u64;

        property
    }
}

// upstream: drm_property.c drm_property_create_bool()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `name` must point to a valid NUL-terminated C string.
/// - Caller must hold `dev->mode_config.mutex` (or call during early device registration).
pub unsafe extern "C" fn drm_property_create_bool(
    dev: *mut DrmDevice,
    flags: u32,
    name: *const c_char,
) -> *mut DrmProperty {
    // SAFETY: `dev` and `name` satisfy preconditions of `drm_property_create_range` per function contract for creating a boolean property with range [0, 1].
    unsafe { drm_property_create_range(dev, flags, name, 0, 1) }
}

// upstream: drm_property.c drm_property_add_enum()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `property` must be non-null and point to an initialized, live `DrmProperty` of type ENUM or BITMASK.
/// - `name` must be non-null and point to a readable NUL-terminated C string.
/// - Caller must ensure exclusive mutable access to `property->enum_list` and `property->values`.
pub unsafe extern "C" fn drm_property_add_enum(
    property: *mut DrmProperty,
    value: u64,
    name: *const c_char,
) -> i32 {
    // SAFETY: `name` is verified to have length < `DRM_PROP_NAME_LEN`; `property` is verified to be ENUM/BITMASK; circular list `enum_list` is walked under caller exclusivity; `index` is bounds-checked against `(*property).num_values` before writing `*(*property).values.add(index)`; new enum object is allocated and linked via `list_add_tail`.
    unsafe {
        let mut index: i32 = 0;

        if warn_on(c_strlen(name) >= DRM_PROP_NAME_LEN) {
            return -EINVAL;
        }

        if warn_on(
            !drm_property_type_is(property, DRM_MODE_PROP_ENUM)
                && !drm_property_type_is(property, DRM_MODE_PROP_BITMASK),
        ) {
            return -EINVAL;
        }

        // Bitmask bit positions are limited to the 64-bit mask.
        if warn_on(drm_property_type_is(property, DRM_MODE_PROP_BITMASK) && value > 63) {
            return -EINVAL;
        }

        let head = core::ptr::addr_of_mut!((*property).enum_list);
        let mut cur = (*head).next;
        while cur != head {
            let prop_enum = enum_from_link(cur);
            if warn_on((*prop_enum).value == value) {
                return -EINVAL;
            }
            index += 1;
            cur = (*cur).next;
        }

        if warn_on(index >= (*property).num_values as i32) {
            return -EINVAL;
        }

        let prop_enum = drm_kapi::memory::kzalloc_obj::<DrmPropertyEnum>();
        if prop_enum.is_null() {
            return -ENOMEM;
        }

        strscpy_pad((*prop_enum).name.as_mut_ptr(), name, DRM_PROP_NAME_LEN);
        (*prop_enum).value = value;
        *(*property).values.add(index as usize) = value;
        list_add_tail(core::ptr::addr_of_mut!((*prop_enum).head), head);

        0
    }
}

/// `list_entry(link, struct drm_property_enum, head)`.
///
/// # Safety
///
/// `link` must be a non-null pointer to the `head` field of an allocated `DrmPropertyEnum` struct
/// so that subtracting `offset_of!(DrmPropertyEnum, head)` yields a valid pointer within the same allocation.
unsafe fn enum_from_link(link: *mut ListHead) -> *mut DrmPropertyEnum {
    // SAFETY: per the function's safety contract, `link` is the `head` member of a live `DrmPropertyEnum`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmPropertyEnum, head))
            .cast::<DrmPropertyEnum>()
    }
}

// upstream: drm_property.c drm_property_destroy()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to the live `DrmDevice` that owns `property`.
/// - `property` must point to a live `DrmProperty` previously created by `drm_property_create`.
/// - Caller must hold `dev->mode_config.mutex` and ensure no concurrent reader or writer accesses `property`.
/// - This function consumes and frees `property` and all its attached enum values.
pub unsafe extern "C" fn drm_property_destroy(dev: *mut DrmDevice, property: *mut DrmProperty) {
    // SAFETY: `property` is unshared per contract; enum list elements are unlinked and freed via `kfree`; `values` array is freed if `num_values != 0`; `property->base` is unregistered from `dev` mode objects; `property->head` is unlinked from `dev->mode_config.property_list` and `property` is freed via `kfree`.
    unsafe {
        let head = core::ptr::addr_of_mut!((*property).enum_list);
        let mut cur = (*head).next;
        while cur != head {
            // list_for_each_entry_safe(): grab the successor before freeing.
            let next = (*cur).next;
            let prop_enum = enum_from_link(cur);
            list_del(cur);
            kfree(prop_enum);
            cur = next;
        }

        if (*property).num_values != 0 {
            kfree((*property).values);
        }

        drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*property).base));
        list_del(core::ptr::addr_of_mut!((*property).head));
        kfree(property);
    }
}

// upstream: drm_property.c drm_mode_getproperty_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - `data` must be non-null, properly aligned, and point to a readable and writable `DrmModeGetProperty` ioctl buffer.
/// - If `file_priv` is non-null, it must point to an initialized, live `DrmFile`.
/// - User pointers inside `data` (`values_ptr`, `enum_blob_ptr`) are written via `put_user` / `copy_to_user_c`, returning `-EFAULT` on failure.
pub unsafe extern "C" fn drm_mode_getproperty_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` is checked non-null and cast to `*mut DrmModeGetProperty` per contract; `dev` and `file_priv` are passed to `drm_property_find`; property fields are read and user-space buffers are populated using fault-checked `put_user` and `copy_to_user_c`.
    unsafe {
        let out_resp = data.cast::<DrmModeGetProperty>();
        let mut enum_count: u32 = 0;
        let mut copied: u32 = 0;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let property = drm_property_find(dev, file_priv, (*out_resp).prop_id);
        if property.is_null() {
            return -ENOENT;
        }

        strscpy_pad(
            (*out_resp).name.as_mut_ptr(),
            (*property).name.as_ptr(),
            DRM_PROP_NAME_LEN,
        );
        (*out_resp).flags = (*property).flags;

        let value_count = (*property).num_values;
        // User addresses are computed as integers: the base may be null or
        // bogus, and only the copy helpers may decide that it faults.
        let values_addr = (*out_resp).values_ptr as usize;
        for i in 0..value_count {
            if i < (*out_resp).count_values
                && put_user(
                    values_addr.wrapping_add(i as usize * size_of::<u64>()) as *mut u64,
                    *(*property).values.add(i as usize),
                ) != 0
            {
                return -EFAULT;
            }
        }
        (*out_resp).count_values = value_count;

        let enum_addr = (*out_resp).enum_blob_ptr as usize;
        if drm_property_type_is(property, DRM_MODE_PROP_ENUM)
            || drm_property_type_is(property, DRM_MODE_PROP_BITMASK)
        {
            let head = core::ptr::addr_of_mut!((*property).enum_list);
            let mut cur = (*head).next;
            while cur != head {
                let prop_enum = enum_from_link(cur);
                enum_count += 1;
                if (*out_resp).count_enum_blobs < enum_count {
                    cur = (*cur).next;
                    continue;
                }

                // Each user entry is a `struct drm_mode_property_enum`
                // (value at offset 0, name following it).
                let user = enum_addr.wrapping_add(copied as usize * USER_PROPERTY_ENUM_SIZE);
                let value = (*prop_enum).value;
                if copy_to_user_c(
                    user.wrapping_add(offset_of!(DrmModePropertyEnum, value)) as *mut c_void,
                    core::ptr::addr_of!(value).cast::<c_void>(),
                    size_of::<u64>(),
                ) != 0
                {
                    return -EFAULT;
                }
                if copy_to_user_c(
                    user.wrapping_add(offset_of!(DrmModePropertyEnum, name)) as *mut c_void,
                    (*prop_enum).name.as_ptr().cast::<c_void>(),
                    DRM_PROP_NAME_LEN,
                ) != 0
                {
                    return -EFAULT;
                }
                copied += 1;
                cur = (*cur).next;
            }
            (*out_resp).count_enum_blobs = enum_count;
        }

        if drm_property_type_is(property, DRM_MODE_PROP_BLOB) {
            (*out_resp).count_enum_blobs = 0;
        }

        0
    }
}

// upstream: drm_property.c drm_property_free_blob()
/// # Safety
///
/// `kref` must point to the embedded `base.refcount` field of a heap-allocated `DrmPropertyBlob`
/// whose reference count has reached zero and is ready for deallocation.
unsafe extern "C" fn drm_property_free_blob(kref: *mut Kref) {
    // SAFETY: `blob_from_kref(kref)` recovers the containing `DrmPropertyBlob`; `head_global` is removed from `dev->mode_config.property_blob_list` under `blob_lock`; `blob->base` is unregistered from mode objects, and the blob allocation is freed via `kvfree`.
    unsafe {
        let blob = blob_from_kref(kref);

        mutex_lock(core::ptr::addr_of_mut!(
            (*(*blob).dev).mode_config.blob_lock
        ));
        list_del(core::ptr::addr_of_mut!((*blob).head_global));
        mutex_unlock(core::ptr::addr_of_mut!(
            (*(*blob).dev).mode_config.blob_lock
        ));

        drm_mode_object_unregister((*blob).dev, core::ptr::addr_of_mut!((*blob).base));
        kvfree(blob.cast::<c_void>());
    }
}

// upstream: drm_property.c drm_property_create_blob()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - If `data` is non-null and `length > 0`, `data` must point to at least `length` readable bytes that remain valid and unmodified during the call.
/// - If successful, returns a newly allocated `DrmPropertyBlob` with an initial reference count of 1.
pub unsafe extern "C" fn drm_property_create_blob(
    dev: *mut DrmDevice,
    length: usize,
    data: *const c_void,
) -> *mut DrmPropertyBlob {
    // SAFETY: `length` is checked against 0 and overflow; `kzalloc` allocates contiguous memory for `DrmPropertyBlob` plus `length` payload bytes; `copy_nonoverlapping` safely copies `length` bytes from `data` into newly allocated `(*blob).data`; mode object registration and insertion into `property_blob_list` under `blob_lock` complete the setup.
    unsafe {
        if length == 0 || length > (i32::MAX as usize) - size_of::<DrmPropertyBlob>() {
            return err_ptr_c(-EINVAL);
        }

        let blob =
            kzalloc(size_of::<DrmPropertyBlob>() + length, GFP_KERNEL).cast::<DrmPropertyBlob>();
        if blob.is_null() {
            return err_ptr_c(-ENOMEM);
        }

        // This must be explicitly initialised, so we can safely call list_del
        // on it in the error path.
        init_list_head(core::ptr::addr_of_mut!((*blob).head_file));

        (*blob).data = blob
            .cast::<u8>()
            .add(size_of::<DrmPropertyBlob>())
            .cast::<c_void>();
        (*blob).length = length;
        (*blob).dev = dev;

        if !data.is_null() {
            core::ptr::copy_nonoverlapping(data.cast::<u8>(), (*blob).data.cast::<u8>(), length);
        }

        let ret = __drm_mode_object_add(
            dev,
            core::ptr::addr_of_mut!((*blob).base),
            DRM_MODE_OBJECT_BLOB,
            true,
            Some(drm_property_free_blob),
        );
        if ret != 0 {
            kvfree(blob.cast::<c_void>());
            return err_ptr_c(-EINVAL);
        }

        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.blob_lock));
        list_add_tail(
            core::ptr::addr_of_mut!((*blob).head_global),
            core::ptr::addr_of_mut!((*dev).mode_config.property_blob_list),
        );
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.blob_lock));

        blob
    }
}

/// `err_ptr()` in pointer form, as used by the blob constructors.
fn err_ptr_c<T>(error: i32) -> *mut T {
    drm_kapi::config::err_ptr::<T>(error)
}

/// `ptr_err()`/`is_err()` in pointer form.
fn is_err_c<T>(ptr: *mut T) -> bool {
    drm_kapi::config::is_err(ptr)
}

fn ptr_err_c<T>(ptr: *mut T) -> i32 {
    drm_kapi::config::ptr_err(ptr)
}

// upstream: drm_property.c drm_property_blob_put()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `blob` may be null (no-op).
/// - If non-null, `blob` must point to a live `DrmPropertyBlob` holding a valid reference count.
///   Decrements the reference count and invokes `drm_property_free_blob` if it drops to zero.
pub unsafe extern "C" fn drm_property_blob_put(blob: *mut DrmPropertyBlob) {
    // SAFETY: if `blob` is non-null, `addr_of_mut!((*blob).base)` gets a valid pointer to its embedded `DrmModeObject` whose refcount is decremented by `drm_mode_object_put`.
    unsafe {
        if blob.is_null() {
            return;
        }
        drm_mode_object_put(core::ptr::addr_of_mut!((*blob).base));
    }
}

// upstream: drm_property.c drm_property_destroy_user_blobs()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `file_priv` must be non-null and point to a live `DrmFile` being closed, with exclusive access to `(*file_priv).blobs`.
/// - Drops file-owned references to all blobs attached to `file_priv`.
pub unsafe extern "C" fn drm_property_destroy_user_blobs(
    _dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
) {
    // SAFETY: `(*file_priv).blobs` list is traversed safely by caching `(*cur).next` before unlinking; each node's containing `DrmPropertyBlob` is unlinked with `list_del_init` and its file reference dropped with `drm_property_blob_put`.
    unsafe {
        let head = core::ptr::addr_of_mut!((*file_priv).blobs);
        let mut cur = (*head).next;
        while cur != head {
            // list_for_each_entry_safe(): grab the successor before unlinking.
            let next = (*cur).next;
            let blob = cur
                .cast::<u8>()
                .sub(offset_of!(DrmPropertyBlob, head_file))
                .cast::<DrmPropertyBlob>();
            list_del_init(cur);
            drm_property_blob_put(blob);
            cur = next;
        }
    }
}

// upstream: drm_property.c drm_property_blob_get()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `blob` must be non-null, properly aligned, and point to a live `DrmPropertyBlob` with a non-zero reference count.
/// - Increments the reference count and returns `blob`. Caller is responsible for eventually dropping it with `drm_property_blob_put`.
pub unsafe extern "C" fn drm_property_blob_get(blob: *mut DrmPropertyBlob) -> *mut DrmPropertyBlob {
    // SAFETY: `blob` is non-null and valid per contract; `addr_of_mut!((*blob).base)` accesses its embedded `DrmModeObject` to increment the reference count via `drm_mode_object_get`.
    unsafe {
        drm_mode_object_get(core::ptr::addr_of_mut!((*blob).base));
        blob
    }
}

// upstream: drm_property.c drm_property_lookup_blob()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - If a blob with `id` is found, its reference count is incremented and returned. The caller acquires ownership of this reference and must release it with `drm_property_blob_put`. Returns null if not found.
pub unsafe extern "C" fn drm_property_lookup_blob(
    dev: *mut DrmDevice,
    id: u32,
) -> *mut DrmPropertyBlob {
    // SAFETY: `dev` is a live `DrmDevice`; `__drm_mode_object_find` looks up the `DRM_MODE_OBJECT_BLOB` and acquires a reference; `obj_to_blob` safely converts the found `DrmModeObject` pointer to enclosing `DrmPropertyBlob`.
    unsafe {
        let obj = __drm_mode_object_find(dev, core::ptr::null_mut(), id, DRM_MODE_OBJECT_BLOB);
        let mut blob: *mut DrmPropertyBlob = core::ptr::null_mut();
        if !obj.is_null() {
            blob = obj_to_blob(obj);
        }
        blob
    }
}

// upstream: drm_property.c drm_property_replace_global_blob()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `replace` must be non-null, properly aligned, and point to a pointer slot holding either null or a live `DrmPropertyBlob` reference. Caller must ensure exclusive access to `*replace`.
/// - If `length != 0` and `data` is non-null, `data` must point to at least `length` readable bytes that remain valid and unmodified during the call.
/// - If `obj_holds_id` is non-null, it must point to a live `DrmModeObject`, and `prop_holds_id` must point to a live `DrmProperty`.
pub unsafe extern "C" fn drm_property_replace_global_blob(
    dev: *mut DrmDevice,
    replace: *mut *mut DrmPropertyBlob,
    length: usize,
    data: *const c_void,
    obj_holds_id: *mut DrmModeObject,
    prop_holds_id: *mut DrmProperty,
) -> i32 {
    // SAFETY: `replace` is verified non-null; `*replace` (old blob) is read under caller exclusivity; newly created blob (if requested) is registered and its ID stored via `drm_object_property_set_value`; on success `old_blob` reference is dropped and `*replace` updated to `new_blob`.
    unsafe {
        let mut new_blob: *mut DrmPropertyBlob = core::ptr::null_mut();

        warn_on(replace.is_null());

        let old_blob = *replace;

        if length != 0 && !data.is_null() {
            new_blob = drm_property_create_blob(dev, length, data);
            if is_err_c(new_blob) {
                return ptr_err_c(new_blob);
            }
        }

        if !obj_holds_id.is_null() {
            let ret = drm_object_property_set_value(
                obj_holds_id,
                prop_holds_id,
                if new_blob.is_null() {
                    0
                } else {
                    (*new_blob).base.id as u64
                },
            );
            if ret != 0 {
                drm_property_blob_put(new_blob);
                return ret;
            }
        }

        drm_property_blob_put(old_blob);
        *replace = new_blob;

        0
    }
}

// upstream: drm_property.c drm_property_replace_blob()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `blob` must be non-null, properly aligned, and point to a pointer slot holding either null or a live `DrmPropertyBlob` reference. Caller must ensure exclusive access to `*blob`.
/// - `new_blob` may be null or point to a live `DrmPropertyBlob`.
/// - Drops the reference held by `*blob` and, if `new_blob` is non-null, takes a new reference on `new_blob` and stores it into `*blob`.
pub unsafe extern "C" fn drm_property_replace_blob(
    blob: *mut *mut DrmPropertyBlob,
    new_blob: *mut DrmPropertyBlob,
) -> bool {
    // SAFETY: `blob` points to an exclusively accessible slot holding `old_blob`; if `old_blob != new_blob`, `old_blob` is dropped via `drm_property_blob_put`, `new_blob` has its reference count incremented via `drm_property_blob_get`, and `*blob` is updated.
    unsafe {
        let old_blob = *blob;

        if old_blob == new_blob {
            return false;
        }

        drm_property_blob_put(old_blob);
        if !new_blob.is_null() {
            drm_property_blob_get(new_blob);
        }

        *blob = new_blob;
        true
    }
}

// upstream: drm_property.c drm_property_replace_blob_from_id()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - `blob` must be non-null, properly aligned, and point to a pointer slot holding either null or a live `DrmPropertyBlob` reference.
/// - `replaced` must be non-null, properly aligned, and point to a writable `bool`.
/// - Caller must ensure exclusive access to `*blob` and `*replaced` without concurrent access.
pub unsafe extern "C" fn drm_property_replace_blob_from_id(
    dev: *mut DrmDevice,
    blob: *mut *mut DrmPropertyBlob,
    blob_id: u64,
    max_size: isize,
    expected_size: isize,
    expected_elem_size: isize,
    replaced: *mut bool,
) -> i32 {
    // SAFETY: `dev` is a live `DrmDevice`; if `blob_id != 0`, `drm_property_lookup_blob` acquires a temporary blob reference that is verified against length constraints and released with `drm_property_blob_put`; `drm_property_replace_blob` safely replaces the reference in `*blob` and `*replaced` is updated under caller exclusivity.
    unsafe {
        let mut new_blob: *mut DrmPropertyBlob = core::ptr::null_mut();

        if blob_id != 0 {
            new_blob = drm_property_lookup_blob(dev, blob_id as u32);
            if new_blob.is_null() {
                drm_dbg_atomic!(dev, "cannot find blob ID %llu\n", blob_id);
                return -EINVAL;
            }

            if max_size > 0 && (*new_blob).length as isize > max_size {
                drm_dbg_atomic!(
                    dev,
                    "[BLOB:%d] length %zu greater than max %zu\n",
                    (*new_blob).base.id,
                    (*new_blob).length,
                    max_size
                );
                drm_property_blob_put(new_blob);
                return -EINVAL;
            }

            if expected_size > 0 && (*new_blob).length as isize != expected_size {
                drm_dbg_atomic!(
                    dev,
                    "[BLOB:%d] length %zu different from expected %zu\n",
                    (*new_blob).base.id,
                    (*new_blob).length,
                    expected_size
                );
                drm_property_blob_put(new_blob);
                return -EINVAL;
            }

            if expected_elem_size > 0 && (*new_blob).length as isize % expected_elem_size != 0 {
                drm_dbg_atomic!(
                    dev,
                    "[BLOB:%d] length %zu not divisible by element size %zu\n",
                    (*new_blob).base.id,
                    (*new_blob).length,
                    expected_elem_size
                );
                drm_property_blob_put(new_blob);
                return -EINVAL;
            }
        }

        *replaced |= drm_property_replace_blob(blob, new_blob);
        drm_property_blob_put(new_blob);

        0
    }
}

// upstream: drm_property.c drm_mode_getblob_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - `data` must be non-null, properly aligned, and point to a readable and writable `DrmModeGetBlob` ioctl structure.
/// - The user-space destination buffer referenced by `data` is written via `copy_to_user_c` and returns `-EFAULT` if inaccessible.
pub unsafe extern "C" fn drm_mode_getblob_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` is cast to `*mut DrmModeGetBlob` per contract; `drm_property_lookup_blob` acquires a reference to the blob matching `blob_id`; payload bytes are safely copied to user space via fault-checked `copy_to_user_c`, and the lookup reference is released with `drm_property_blob_put`.
    unsafe {
        let out_resp = data.cast::<DrmModeGetBlob>();
        let mut ret = 0;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let blob = drm_property_lookup_blob(dev, (*out_resp).blob_id);
        if blob.is_null() {
            return -ENOENT;
        }

        'unref: {
            if (*out_resp).length as usize == (*blob).length
                && copy_to_user_c(
                    ((*out_resp).data as usize) as *mut c_void,
                    (*blob).data,
                    (*blob).length,
                ) != 0
            {
                ret = -EFAULT;
                break 'unref;
            }
            (*out_resp).length = (*blob).length as u32;
        }

        drm_property_blob_put(blob);
        ret
    }
}

// upstream: drm_property.c drm_mode_createblob_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - `data` must be non-null, properly aligned, and point to a readable and writable `DrmModeCreateBlob` ioctl structure.
/// - `file_priv` must be non-null and point to a live `DrmFile` structure.
/// - User payload referenced by `data` is read via `copy_from_user_c` and returns `-EFAULT` if inaccessible.
pub unsafe extern "C" fn drm_mode_createblob_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` is cast to `*mut DrmModeCreateBlob` per contract; `drm_property_create_blob` allocates a new blob with refcount 1; `copy_from_user_c` copies `length` bytes from user space into `(*blob).data`; `blob->head_file` is appended to `(*file_priv).blobs` under `blob_lock`.
    unsafe {
        let out_resp = data.cast::<DrmModeCreateBlob>();

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let blob = drm_property_create_blob(dev, (*out_resp).length as usize, core::ptr::null());
        if is_err_c(blob) {
            return ptr_err_c(blob);
        }

        if copy_from_user_c(
            (*blob).data,
            ((*out_resp).data as usize) as *const c_void,
            (*out_resp).length as usize,
        ) != 0
        {
            drm_property_blob_put(blob);
            return -EFAULT;
        }

        // Dropping the lock between create_blob and our access here is safe
        // because we only ever add to the list, and the blob is ours.
        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.blob_lock));
        (*out_resp).blob_id = (*blob).base.id;
        list_add_tail(
            core::ptr::addr_of_mut!((*blob).head_file),
            core::ptr::addr_of_mut!((*file_priv).blobs),
        );
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.blob_lock));

        0
    }
}

// upstream: drm_property.c drm_mode_destroyblob_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be non-null and point to an initialized, live `DrmDevice`.
/// - `data` must be non-null, properly aligned, and point to a readable `DrmModeDestroyBlob` ioctl structure.
/// - `file_priv` must be non-null and point to a live `DrmFile` structure.
pub unsafe extern "C" fn drm_mode_destroyblob_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` is cast to `*mut DrmModeDestroyBlob` per contract; `drm_property_lookup_blob` acquires a reference; `file_priv->blobs` list is searched under `blob_lock` to verify ownership; if found, `head_file` is unlinked with `list_del_init` and both the lookup and file references are dropped via `drm_property_blob_put`.
    unsafe {
        let out_resp = data.cast::<DrmModeDestroyBlob>();
        let mut found = false;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let blob = drm_property_lookup_blob(dev, (*out_resp).blob_id);
        if blob.is_null() {
            return -ENOENT;
        }

        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.blob_lock));

        // Ensure the property was actually created by this user.
        let head = core::ptr::addr_of_mut!((*file_priv).blobs);
        let mut cur = (*head).next;
        while cur != head {
            let bt = cur
                .cast::<u8>()
                .sub(offset_of!(DrmPropertyBlob, head_file))
                .cast::<DrmPropertyBlob>();
            if bt == blob {
                found = true;
                break;
            }
            cur = (*cur).next;
        }

        if !found {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.blob_lock));
            drm_property_blob_put(blob);
            return -EPERM;
        }

        // We must drop head_file here, because we may not be the last
        // reference holder.
        list_del_init(core::ptr::addr_of_mut!((*blob).head_file));
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.blob_lock));

        // One reference from lookup, and one from the filp.
        drm_property_blob_put(blob);
        drm_property_blob_put(blob);
        0
    }
}

// upstream: drm_property.c drm_property_change_valid_get()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `property` must be non-null and point to an initialized, live `DrmProperty`.
/// - `ref` must be non-null, properly aligned, and point to a writable pointer slot. Caller must ensure exclusive access to `*ref`.
/// - If returning `true` and `*ref` is non-null, caller receives an acquired reference to the mode object that must be released via `drm_property_change_valid_put`.
pub unsafe extern "C" fn drm_property_change_valid_get(
    property: *mut DrmProperty,
    value: u64,
    r#ref: *mut *mut DrmModeObject,
) -> bool {
    // SAFETY: `property` and `ref` are valid per contract; `*ref` is initialized to null; property type and values array are read without concurrent mutation; for blob and object types, mode object lookup acquires a reference stored in `*ref`.
    unsafe {
        if (*property).flags & DRM_MODE_PROP_IMMUTABLE != 0 {
            return false;
        }

        *r#ref = core::ptr::null_mut();

        if drm_property_type_is(property, DRM_MODE_PROP_RANGE) {
            if value < *(*property).values.add(0) || value > *(*property).values.add(1) {
                return false;
            }
            return true;
        } else if drm_property_type_is(property, DRM_MODE_PROP_SIGNED_RANGE) {
            let svalue = value as i64;
            if svalue < *(*property).values.add(0) as i64
                || svalue > *(*property).values.add(1) as i64
            {
                return false;
            }
            return true;
        } else if drm_property_type_is(property, DRM_MODE_PROP_BITMASK) {
            let mut valid_mask: u64 = 0;
            for i in 0..(*property).num_values as usize {
                valid_mask |= 1u64 << *(*property).values.add(i);
            }
            return value & !valid_mask == 0;
        } else if drm_property_type_is(property, DRM_MODE_PROP_BLOB) {
            if value == 0 {
                return true;
            }
            let blob = drm_property_lookup_blob((*property).dev, value as u32);
            if !blob.is_null() {
                *r#ref = core::ptr::addr_of_mut!((*blob).base);
                return true;
            } else {
                return false;
            }
        } else if drm_property_type_is(property, DRM_MODE_PROP_OBJECT) {
            // a zero value for an object property translates to null:
            if value == 0 {
                return true;
            }

            *r#ref = __drm_mode_object_find(
                (*property).dev,
                core::ptr::null_mut(),
                value as u32,
                *(*property).values.add(0) as u32,
            );
            return !(*r#ref).is_null();
        }

        for i in 0..(*property).num_values as usize {
            if *(*property).values.add(i) == value {
                return true;
            }
        }
        false
    }
}

// upstream: drm_property.c drm_property_change_valid_put()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `property` must be non-null and point to an initialized, live `DrmProperty`.
/// - `ref` may be null (no-op). If non-null, it must be a live mode object reference previously returned by `drm_property_change_valid_get` for this property.
pub unsafe extern "C" fn drm_property_change_valid_put(
    property: *mut DrmProperty,
    r#ref: *mut DrmModeObject,
) {
    // SAFETY: `ref` is checked non-null; `property` flags determine whether `ref` is an object (dropped via `drm_mode_object_put`) or a blob (converted via `obj_to_blob` and dropped via `drm_property_blob_put`).
    unsafe {
        if r#ref.is_null() {
            return;
        }

        if drm_property_type_is(property, DRM_MODE_PROP_OBJECT) {
            drm_mode_object_put(r#ref);
        } else if drm_property_type_is(property, DRM_MODE_PROP_BLOB) {
            drm_property_blob_put(obj_to_blob(r#ref));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        drm_mode_object_upstream::drm_mode_object_find, test_support::TestDrm,
        uapi_drm_consts::DRM_MODE_OBJECT_PROPERTY,
    };

    #[test]
    fn enum_property_has_values_in_insertion_order() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        // SAFETY: `t` provides a valid stack-allocated `TestDrm` fixture whose pointer `dev` remains live for the test; calls exercise enum property creation, addition, lookup, and destruction.
        unsafe {
            let prop = drm_property_create(dev, DRM_MODE_PROP_ENUM, b"mode\0".as_ptr().cast(), 2);
            assert!(!prop.is_null());
            assert_eq!((*prop).num_values, 2);
            assert_eq!(
                drm_property_add_enum(prop, 7, b"seven\0".as_ptr().cast()),
                0
            );
            assert_eq!(drm_property_add_enum(prop, 9, b"nine\0".as_ptr().cast()), 0);
            assert_eq!(*(*prop).values.add(0), 7);
            assert_eq!(*(*prop).values.add(1), 9);
            // A duplicate value is rejected (the C WARN_ON path).
            assert_eq!(
                drm_property_add_enum(prop, 7, b"again\0".as_ptr().cast()),
                -EINVAL
            );
            // The property is registered as an object and listed on the device.
            let found = drm_mode_object_find(
                dev,
                core::ptr::null_mut(),
                (*prop).base.id,
                DRM_MODE_OBJECT_PROPERTY,
            );
            assert_eq!(obj_to_property(found), prop);
            drm_property_destroy(dev, prop);
        }
    }

    #[test]
    fn range_property_accepts_only_in_range_values() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        // SAFETY: `t` provides a valid stack-allocated `TestDrm` fixture whose pointer `dev` remains live; calls test range property creation, validation, reference drop, and destruction.
        unsafe {
            let prop = drm_property_create_range(dev, 0, b"range\0".as_ptr().cast(), 0, 100);
            assert!(!prop.is_null());
            let mut r: *mut DrmModeObject = core::ptr::null_mut();
            assert!(drm_property_change_valid_get(prop, 50, &mut r));
            drm_property_change_valid_put(prop, r);
            assert!(!drm_property_change_valid_get(prop, 101, &mut r));
            assert!(r.is_null());
            drm_property_destroy(dev, prop);
        }
    }

    #[test]
    fn bitmask_property_counts_its_bits() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        let list = [
            DrmPropEnumList {
                r#type: 0,
                name: b"a\0".as_ptr().cast::<c_char>() as *mut c_char,
            },
            DrmPropEnumList {
                r#type: 2,
                name: b"c\0".as_ptr().cast::<c_char>() as *mut c_char,
            },
        ];
        // SAFETY: `t` provides a valid stack-allocated `TestDrm` fixture; `list` elements contain valid static C string literals; tests bitmask property creation and cleanup.
        unsafe {
            let prop = drm_property_create_bitmask(
                dev,
                0,
                b"bits\0".as_ptr().cast(),
                list.as_ptr(),
                2,
                0b101,
            );
            assert!(!prop.is_null());
            assert_eq!((*prop).num_values, 2);
            assert_ne!((*prop).flags & DRM_MODE_PROP_BITMASK, 0);
            drm_property_destroy(dev, prop);
        }
    }

    #[test]
    fn flags_must_have_exactly_one_type() {
        assert!(drm_property_flags_valid(DRM_MODE_PROP_RANGE));
        assert!(!drm_property_flags_valid(0));
        assert!(!drm_property_flags_valid(
            DRM_MODE_PROP_RANGE | DRM_MODE_PROP_ENUM
        ));
        assert!(!drm_property_flags_valid(DRM_MODE_PROP_RANGE | (1 << 3)));
    }

    #[test]
    fn blob_lives_until_its_last_reference_is_dropped() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        let payload = [1u8, 2, 3, 4];
        // SAFETY: `t` provides a valid stack-allocated `TestDrm` fixture; `payload` is a valid stack byte array; tests blob creation, inspection, refcounting, and destruction.
        unsafe {
            let blob = drm_property_create_blob(dev, payload.len(), payload.as_ptr().cast());
            assert!(!is_err_c(blob));
            assert_eq!((*blob).length, 4);
            assert_eq!(
                core::slice::from_raw_parts((*blob).data.cast::<u8>(), 4),
                &payload
            );
            let id = (*blob).base.id;

            // Lookup takes a reference; the creator's reference is the first.
            let again = drm_property_lookup_blob(dev, id);
            assert_eq!(again, blob);
            drm_property_blob_put(again);
            assert_eq!(drm_property_lookup_blob(dev, id), blob);
            drm_property_blob_put(blob);

            drm_property_blob_put(blob);
            // Last reference dropped: the id is gone.
            assert!(drm_property_lookup_blob(dev, id).is_null());
        }
    }

    #[test]
    fn blob_creation_rejects_empty_payload() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        // SAFETY: `t` provides a valid stack-allocated `TestDrm` fixture; tests rejection and error code on a 0-length payload with null data pointer.
        unsafe {
            let blob = drm_property_create_blob(dev, 0, core::ptr::null());
            assert!(is_err_c(blob));
            assert_eq!(ptr_err_c(blob), -EINVAL);
        }
    }
}
