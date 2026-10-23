// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_blend.c`.
//!
//! Source-faithful transcription of Linux 7.2.3. Property creation and
//! atomic state lookup are declared as C ABI references and resolved by the
//! other modules in this shared crate.

use alloc::format;
use core::ffi::c_char;

use drm_kapi::{
    assert::warn_on,
    config::{EINVAL, ENOMEM, is_err, ptr_err},
    print::{DrmLogLevel, drm_log_at},
};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_device_types::DrmDevice,
    drm_mode_object_types::DrmModeObject,
    drm_plane_types::{DrmPlane, DrmPlaneState},
    drm_property_types::{DrmPropEnumList, DrmProperty},
    uapi_drm_consts::*,
};

// upstream: include/drm/drm_blend.h
pub const DRM_BLEND_ALPHA_OPAQUE: u32 = 0xffff;
pub const DRM_MODE_BLEND_PREMULTI: u32 = 0;
pub const DRM_MODE_BLEND_COVERAGE: u32 = 1;
pub const DRM_MODE_BLEND_PIXEL_NONE: u32 = 2;

// Property and atomic helpers are implemented by sibling DRM modules.
unsafe extern "C" {
    fn drm_property_create_range(
        dev: *mut DrmDevice,
        flags: u32,
        name: *const c_char,
        min: u64,
        max: u64,
    ) -> *mut DrmProperty;
    fn drm_property_create(
        dev: *mut DrmDevice,
        flags: u32,
        name: *const c_char,
        num_values: i32,
    ) -> *mut DrmProperty;
    fn drm_property_create_bitmask(
        dev: *mut DrmDevice,
        flags: u32,
        name: *const c_char,
        props: *const DrmPropEnumList,
        num_props: i32,
        supported_bits: u64,
    ) -> *mut DrmProperty;
    fn drm_property_add_enum(property: *mut DrmProperty, value: u64, name: *const c_char) -> i32;
    fn drm_property_destroy(dev: *mut DrmDevice, property: *mut DrmProperty);
    fn drm_object_attach_property(
        obj: *mut DrmModeObject,
        property: *mut DrmProperty,
        init_val: u64,
    );
    fn drm_atomic_get_plane_state(
        state: *mut DrmAtomicCommit,
        plane: *mut DrmPlane,
    ) -> *mut DrmPlaneState;
}

// `drm_dbg_atomic()` routed to the kernel log.
fn drm_dbg_atomic(dev: *mut DrmDevice, msg: &str) {
    let _ = dev;
    drm_log_at(DrmLogLevel::Debug, "drm_atomic", file!(), line!(), msg);
}

// upstream: include/drm/drm_blend.h BIT(), __builtin_ffs()
#[inline]
fn ffs(x: u32) -> u32 {
    if x == 0 { 0 } else { x.trailing_zeros() + 1 }
}

// Name of a `char *name` field for log output.
fn cstr_name<'a>(name: *const c_char) -> &'a str {
    if name.is_null() {
        return "";
    }
    // SAFETY: `name` was checked non-null above and points to a null-terminated C string in valid memory.
    unsafe { core::ffi::CStr::from_ptr(name) }
        .to_str()
        .unwrap_or("")
}

// upstream: include/linux/log2.h is_power_of_2()
#[inline]
fn is_power_of_2(x: u32) -> bool {
    x != 0 && (x & (x - 1)) == 0
}

// upstream: drm_crtc/drm_plane zpos_changed bit-field (bit 4 of the first byte)
#[inline]
fn crtc_state_zpos_changed_set(s: *mut DrmCrtcState, v: bool) {
    // SAFETY: `s` is a valid pointer to an initialized `DrmCrtcState` with valid bitfield array.
    unsafe {
        let b = &mut (*s)._bitfields_10[0];
        if v { *b |= 1 << 4 } else { *b &= !(1 << 4) }
    }
}

// upstream: drm_blend.c drm_plane_create_alpha_property()
#[unsafe(no_mangle)]
/// # Safety
/// `plane` must point to an initialized `DrmPlane` with a valid, live `dev` pointer.
/// Caller must ensure exclusive access to `plane` during setup (e.g. before device registration
/// or under modeset configuration locks). If `plane->state` is non-null, it must point to an
/// initialized, mutable `DrmPlaneState`.
pub unsafe extern "C" fn drm_plane_create_alpha_property(plane: *mut DrmPlane) -> i32 {
    // SAFETY: `plane` is a valid, aligned, non-null pointer to DrmPlane with valid `plane.dev`.
    let plane = unsafe { &mut *plane };
    // SAFETY: `plane.dev` is valid and string literal `c"alpha"` is a valid null-terminated C string.
    let prop = unsafe {
        drm_property_create_range(
            plane.dev,
            0,
            c"alpha".as_ptr(),
            0,
            DRM_BLEND_ALPHA_OPAQUE as u64,
        )
    };
    if prop.is_null() {
        return -ENOMEM;
    }

    // SAFETY: `plane.base` is a valid DrmModeObject and `prop` was successfully created above.
    unsafe { drm_object_attach_property(&mut plane.base, prop, DRM_BLEND_ALPHA_OPAQUE as u64) };
    plane.alpha_property = prop;
    if !plane.state.is_null() {
        // SAFETY: `plane.state` was checked non-null above; writing default opaque alpha into the plane state.
        unsafe { (*plane.state).alpha = DRM_BLEND_ALPHA_OPAQUE as u16 };
    }
    0
}

// upstream: drm_blend.c drm_plane_create_rotation_property()
#[unsafe(no_mangle)]
/// # Safety
/// `plane` must point to an initialized `DrmPlane` with a valid, live `dev` pointer.
/// Caller must ensure exclusive access to `plane` during setup (e.g. before device registration).
/// If `plane->state` is non-null, it must point to an initialized, mutable `DrmPlaneState`.
pub unsafe extern "C" fn drm_plane_create_rotation_property(
    plane: *mut DrmPlane,
    rotation: u32,
    supported_rotations: u32,
) -> i32 {
    // __builtin_ffs(X) - 1
    let props: [DrmPropEnumList; 6] = [
        DrmPropEnumList {
            r#type: (ffs(DRM_MODE_ROTATE_0) - 1) as i32,
            name: c"rotate-0".as_ptr() as *mut c_char,
        },
        DrmPropEnumList {
            r#type: (ffs(DRM_MODE_ROTATE_90) - 1) as i32,
            name: c"rotate-90".as_ptr() as *mut c_char,
        },
        DrmPropEnumList {
            r#type: (ffs(DRM_MODE_ROTATE_180) - 1) as i32,
            name: c"rotate-180".as_ptr() as *mut c_char,
        },
        DrmPropEnumList {
            r#type: (ffs(DRM_MODE_ROTATE_270) - 1) as i32,
            name: c"rotate-270".as_ptr() as *mut c_char,
        },
        DrmPropEnumList {
            r#type: (ffs(DRM_MODE_REFLECT_X) - 1) as i32,
            name: c"reflect-x".as_ptr() as *mut c_char,
        },
        DrmPropEnumList {
            r#type: (ffs(DRM_MODE_REFLECT_Y) - 1) as i32,
            name: c"reflect-y".as_ptr() as *mut c_char,
        },
    ];
    // SAFETY: dereferencing caller-provided plane pointer to attach rotation property.
    let plane_ref = unsafe { &mut *plane };

    warn_on((supported_rotations & DRM_MODE_ROTATE_MASK) == 0);
    warn_on(!is_power_of_2(rotation & DRM_MODE_ROTATE_MASK));
    warn_on(rotation & !supported_rotations != 0);

    // SAFETY: `plane_ref.dev` is valid, string is null-terminated, and `props` array has 6 valid entries.
    let prop = unsafe {
        drm_property_create_bitmask(
            plane_ref.dev,
            0,
            c"rotation".as_ptr(),
            props.as_ptr(),
            props.len() as i32,
            supported_rotations as u64,
        )
    };
    if prop.is_null() {
        return -ENOMEM;
    }

    // SAFETY: `plane_ref.base` is a valid DrmModeObject and `prop` is a valid non-null property.
    unsafe { drm_object_attach_property(&mut plane_ref.base, prop, rotation as u64) };
    if !plane_ref.state.is_null() {
        // SAFETY: `plane_ref.state` was checked non-null and points to the plane's active DrmPlaneState.
        unsafe { (*plane_ref.state).rotation = rotation };
    }
    plane_ref.rotation_property = prop;
    0
}

// upstream: drm_blend.c drm_rotation_simplify()
#[unsafe(no_mangle)]
pub extern "C" fn drm_rotation_simplify(mut rotation: u32, supported_rotations: u32) -> u32 {
    if rotation & !supported_rotations != 0 {
        rotation ^= DRM_MODE_REFLECT_X | DRM_MODE_REFLECT_Y;
        rotation = (rotation & DRM_MODE_REFLECT_MASK)
            | (1u32 << ((ffs(rotation & DRM_MODE_ROTATE_MASK) + 1) % 4));
    }
    rotation
}

// upstream: drm_blend.c drm_plane_create_zpos_property()
#[unsafe(no_mangle)]
/// # Safety
/// - `plane` must point to an initialized `DrmPlane` with a valid, live `dev` pointer.
/// - `min <= zpos && zpos <= max` must hold for the default and range boundaries.
/// - Caller must ensure exclusive access or synchronization under modeset locks.
/// - If `plane->state` is non-null, it must point to an initialized, mutable `DrmPlaneState`.
pub unsafe extern "C" fn drm_plane_create_zpos_property(
    plane: *mut DrmPlane,
    zpos: u32,
    min: u32,
    max: u32,
) -> i32 {
    // SAFETY: dereferencing caller-provided plane pointer to attach mutable zpos range property.
    let plane = unsafe { &mut *plane };
    // SAFETY: `plane.dev` points to the owning DrmDevice; creating mutable range property c"zpos" with bounds [min, max].
    let prop = unsafe {
        drm_property_create_range(plane.dev, 0, c"zpos".as_ptr(), min as u64, max as u64)
    };
    if prop.is_null() {
        return -ENOMEM;
    }

    // SAFETY: `plane.base` is a valid DrmModeObject; attaching zpos property with initial value.
    unsafe { drm_object_attach_property(&mut plane.base, prop, zpos as u64) };
    plane.zpos_property = prop;
    if !plane.state.is_null() {
        // SAFETY: `plane.state` was checked non-null above; initializing both zpos and normalized_zpos with caller-provided default zpos.
        unsafe {
            (*plane.state).zpos = zpos;
            (*plane.state).normalized_zpos = zpos;
        }
    }
    0
}

// upstream: drm_blend.c drm_plane_create_zpos_immutable_property()
#[unsafe(no_mangle)]
/// # Safety
/// - `plane` must point to an initialized `DrmPlane` with a valid, live `dev` pointer.
/// - `zpos` specifies the fixed, immutable zpos value.
/// - Caller must ensure exclusive access or synchronization under modeset locks.
/// - If `plane->state` is non-null, it must point to an initialized, mutable `DrmPlaneState`.
pub unsafe extern "C" fn drm_plane_create_zpos_immutable_property(
    plane: *mut DrmPlane,
    zpos: u32,
) -> i32 {
    // SAFETY: dereferencing caller-provided plane pointer to attach immutable zpos property.
    let plane = unsafe { &mut *plane };
    // SAFETY: `plane.dev` points to the owning DrmDevice; creating immutable range property c"zpos" with fixed range [zpos, zpos].
    let prop = unsafe {
        drm_property_create_range(
            plane.dev,
            DRM_MODE_PROP_IMMUTABLE,
            c"zpos".as_ptr(),
            zpos as u64,
            zpos as u64,
        )
    };
    if prop.is_null() {
        return -ENOMEM;
    }

    // SAFETY: `plane.base` is a valid DrmModeObject; attaching immutable zpos property with fixed value.
    unsafe { drm_object_attach_property(&mut plane.base, prop, zpos as u64) };
    plane.zpos_property = prop;
    if !plane.state.is_null() {
        // SAFETY: `plane.state` was checked non-null above; initializing immutable zpos and normalized_zpos to the fixed value.
        unsafe {
            (*plane.state).zpos = zpos;
            (*plane.state).normalized_zpos = zpos;
        }
    }
    0
}

// upstream: drm_blend.c drm_atomic_commit_zpos_cmp()
// The C comparator returns `sa->zpos - sb->zpos` as int; zpos fits in the
// positive range for all valid planes, so the wrapping difference matches.
fn drm_atomic_commit_zpos_cmp(
    sa: *const DrmPlaneState,
    sb: *const DrmPlaneState,
) -> core::cmp::Ordering {
    // SAFETY: `sa` and `sb` are valid, aligned pointers to `DrmPlaneState` structures.
    let (sa, sb) = unsafe { (&*sa, &*sb) };
    if sa.zpos != sb.zpos {
        let d = (sa.zpos as i32).wrapping_sub(sb.zpos as i32);
        return d.cmp(&0);
    }
    // SAFETY: `sa->plane` and `sb->plane` point to valid `DrmPlane` structures with initialized `base.id`.
    let (ia, ib) = unsafe { ((*sa.plane).base.id, (*sb.plane).base.id) };
    (ia as i32).wrapping_sub(ib as i32).cmp(&0)
}

// upstream: drm_blend.c drm_atomic_helper_crtc_normalize_zpos()
/// # Safety
/// `crtc` must be a valid pointer to an initialized `DrmCrtc` with valid `dev`.
/// `crtc_state` must be a valid pointer to `DrmCrtcState` associated with a valid `state` commit.
unsafe fn drm_atomic_helper_crtc_normalize_zpos(
    crtc: *mut DrmCrtc,
    crtc_state: *mut DrmCrtcState,
) -> i32 {
    // SAFETY: `crtc_state` is a valid pointer to an initialized DrmCrtcState.
    let state = unsafe { (*crtc_state).state };
    // SAFETY: `crtc` is a valid pointer to an initialized DrmCrtc with initialized `dev`.
    let dev = unsafe { (*crtc).dev };
    // SAFETY: `dev` is valid with initialized `mode_config`.
    let total_planes = unsafe { (*dev).mode_config.num_total_plane };
    let mut ret = 0;

    drm_dbg_atomic(
        dev,
        // SAFETY: `crtc` is valid; reading `base.id` and `name` for diagnostic log.
        &format!(
            "[CRTC:{}:{}] calculating normalized zpos values",
            unsafe { (*crtc).base.id },
            cstr_name(unsafe { (*crtc).name })
        ),
    );

    // kmalloc_objs(*states, total_planes)
    let states = drm_kapi::memory::kmalloc(
        core::mem::size_of::<*mut DrmPlaneState>() * total_planes.max(0) as usize,
        drm_kapi::config::GFP_KERNEL,
    ) as *mut *mut DrmPlaneState;
    if states.is_null() {
        return -ENOMEM;
    }

    let mut n: usize = 0;
    // SAFETY: `crtc_state` is valid; reading initialized `plane_mask`.
    let plane_mask = unsafe { (*crtc_state).plane_mask };
    // drm_for_each_plane_mask(plane, dev, mask)
    // SAFETY: `dev->mode_config.plane_list` is an initialized circular list head.
    let head = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.plane_list) };
    // SAFETY: `head` points to initialized ListHead; reading `next`.
    let mut link = unsafe { (*head).next };
    'planes: while link != head {
        // SAFETY: `link` points to `DrmPlane.head` embedded in a live DrmPlane.
        let plane = unsafe {
            link.cast::<u8>().sub(core::mem::offset_of!(DrmPlane, head)) as *mut DrmPlane
        };
        // SAFETY: `link` is a valid ListHead node in `plane_list`.
        link = unsafe { (*link).next };
        // SAFETY: `plane` is a valid DrmPlane with initialized `index`.
        if plane_mask & (1u32 << unsafe { (*plane).index }) == 0 {
            continue;
        }
        // SAFETY: `state` is a valid atomic commit and `plane` is a live plane on the device.
        let plane_state = unsafe { drm_atomic_get_plane_state(state, plane) };
        if is_err(plane_state) {
            ret = ptr_err(plane_state);
            break 'planes;
        }
        // SAFETY: `states` has capacity for `total_planes` entries and `n < total_planes`.
        unsafe { *states.add(n) = plane_state };
        n += 1;
        drm_dbg_atomic(
            dev,
            // SAFETY: `plane` and `plane_state` are valid pointers for diagnostic logging.
            &format!(
                "[PLANE:{}:{}] processing zpos value {}",
                unsafe { (*plane).base.id },
                cstr_name(unsafe { (*plane).name }),
                unsafe { (*plane_state).zpos }
            ),
        );
    }

    if ret == 0 {
        // sort(states, n, sizeof(*states), drm_atomic_commit_zpos_cmp, NULL)
        // SAFETY: `states` points to `n` contiguous initialized `*mut DrmPlaneState` pointers.
        let slice = unsafe { core::slice::from_raw_parts_mut(states, n) };
        slice.sort_by(|a, b| drm_atomic_commit_zpos_cmp(*a, *b));

        for i in 0..n {
            // SAFETY: `i < n` and `states` was populated with `n` elements.
            let st = unsafe { *states.add(i) };
            // SAFETY: `st` points to a valid DrmPlaneState with valid `plane` pointer.
            let plane = unsafe { (*st).plane };
            // SAFETY: `st` points to a mutable DrmPlaneState.
            unsafe { (*st).normalized_zpos = i as u32 };
            drm_dbg_atomic(
                dev,
                // SAFETY: `plane` is valid for diagnostic logging.
                &format!(
                    "[PLANE:{}:{}] normalized zpos value {}",
                    unsafe { (*plane).base.id },
                    cstr_name(unsafe { (*plane).name }),
                    i
                ),
            );
        }
        crtc_state_zpos_changed_set(crtc_state, true);
    }

    // SAFETY: `states` was allocated via `kmalloc` above and is freed on all exit paths.
    unsafe { drm_kapi::memory::kfree(states) };
    ret
}

// upstream: drm_blend.c drm_atomic_normalize_zpos()
#[unsafe(no_mangle)]
/// # Safety
/// `state` must point to an initialized `DrmAtomicCommit` with valid `dev`.
/// `state->planes` must have at least `dev->mode_config.num_total_plane` entries, and
/// `state->crtcs` must have at least `dev->mode_config.num_crtc` entries.
/// Caller must hold the DRM modeset acquire locks for affected CRTCs and planes.
pub unsafe extern "C" fn drm_atomic_normalize_zpos(
    _dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    // SAFETY: `state` is a valid non-null pointer to DrmAtomicCommit with initialized `dev`.
    let state_ref = unsafe { &*state };
    let sdev = state_ref.dev;
    // SAFETY: reading total registered plane count from (*sdev).mode_config under modeset locks.
    let num_total_plane = unsafe { (*sdev).mode_config.num_total_plane };
    // SAFETY: reading total registered CRTC count from (*sdev).mode_config under modeset locks.
    let num_crtc = unsafe { (*sdev).mode_config.num_crtc };
    let ret = 0;

    // for_each_oldnew_plane_in_state()
    for i in 0..num_total_plane.max(0) as usize {
        // SAFETY: `state_ref.planes` contains `num_total_plane` entries.
        let entry = unsafe { &*state_ref.planes.add(i) };
        if entry.ptr.is_null() {
            continue;
        }
        let plane = entry.ptr;
        let old_plane_state = entry.old_state;
        let new_plane_state = entry.new_state;
        let _ = plane;
        // SAFETY: `new_plane_state` is a valid pointer to DrmPlaneState.
        let crtc = unsafe { (*new_plane_state).crtc };
        if crtc.is_null() {
            continue;
        }
        // SAFETY: `old_plane_state` and `new_plane_state` are valid pointers to DrmPlaneState.
        if unsafe { (*old_plane_state).zpos != (*new_plane_state).zpos } {
            // SAFETY: `state` and `crtc` are valid and `crtc->index` indexes into `state->crtcs`.
            let new_crtc_state = unsafe { drm_atomic_get_new_crtc_state_inline(state, crtc) };
            crtc_state_zpos_changed_set(new_crtc_state, true);
        }
    }

    // for_each_oldnew_crtc_in_state()
    for i in 0..num_crtc.max(0) as usize {
        // SAFETY: `state_ref.crtcs` contains `num_crtc` entries.
        let entry = unsafe { &*state_ref.crtcs.add(i) };
        if entry.ptr.is_null() {
            continue;
        }
        let crtc = entry.ptr;
        let old_crtc_state = entry.old_state;
        let new_crtc_state = entry.new_state;
        // SAFETY: `new_crtc_state` is a valid pointer to DrmCrtcState.
        let zpos_changed = unsafe { ((*new_crtc_state)._bitfields_10[0] >> 4) & 1 != 0 };
        // SAFETY: `old_crtc_state` and `new_crtc_state` are valid pointers to DrmCrtcState.
        if unsafe { (*old_crtc_state).plane_mask != (*new_crtc_state).plane_mask } || zpos_changed {
            // SAFETY: `crtc` and `new_crtc_state` are valid; normalizes zpos across its active planes.
            let ret = unsafe { drm_atomic_helper_crtc_normalize_zpos(crtc, new_crtc_state) };
            if ret != 0 {
                return ret;
            }
        }
    }

    ret
}

// upstream: drm_atomic.h drm_atomic_get_new_crtc_state() (header inline,
// indexed by crtc->index)
/// # Safety
/// `state` must point to an active `DrmAtomicCommit` whose `crtcs` array contains at least
/// `crtc->index + 1` entries. `crtc` must be a valid pointer to `DrmCrtc`.
unsafe fn drm_atomic_get_new_crtc_state_inline(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> *mut DrmCrtcState {
    // SAFETY: `crtc` is a valid pointer to DrmCrtc with valid `index`.
    let index = unsafe { (*crtc).index } as usize;
    // SAFETY: `state->crtcs` contains at least `index + 1` entries in atomic state commit.
    unsafe { (*(*state).crtcs.add(index)).new_state }
}

// upstream: drm_blend.c drm_plane_create_blend_mode_property()
#[unsafe(no_mangle)]
/// # Safety
/// `plane` must point to an initialized `DrmPlane` with a valid, live `dev` pointer.
/// Caller must ensure exclusive access or synchronization under modeset configuration locks.
pub unsafe extern "C" fn drm_plane_create_blend_mode_property(
    plane: *mut DrmPlane,
    supported_modes: u32,
) -> i32 {
    // SAFETY: `plane` is a valid pointer to DrmPlane with initialized `dev`.
    let dev = unsafe { (*plane).dev };
    let props: [DrmPropEnumList; 3] = [
        DrmPropEnumList {
            r#type: DRM_MODE_BLEND_PIXEL_NONE as i32,
            name: c"None".as_ptr() as *mut c_char,
        },
        DrmPropEnumList {
            r#type: DRM_MODE_BLEND_PREMULTI as i32,
            name: c"Pre-multiplied".as_ptr() as *mut c_char,
        },
        DrmPropEnumList {
            r#type: DRM_MODE_BLEND_COVERAGE as i32,
            name: c"Coverage".as_ptr() as *mut c_char,
        },
    ];
    let valid_mode_mask: u32 = (1 << DRM_MODE_BLEND_PIXEL_NONE)
        | (1 << DRM_MODE_BLEND_PREMULTI)
        | (1 << DRM_MODE_BLEND_COVERAGE);

    if warn_on(
        (supported_modes & !valid_mode_mask) != 0
            || (supported_modes & (1 << DRM_MODE_BLEND_PREMULTI)) == 0,
    ) {
        return -EINVAL;
    }

    // SAFETY: `dev` is valid and string is null-terminated.
    let prop = unsafe {
        drm_property_create(
            dev,
            0,
            c"pixel blend mode".as_ptr(),
            supported_modes.count_ones() as i32,
        )
    };
    if prop.is_null() {
        return -ENOMEM;
    }

    for p in props.iter() {
        if (1u32 << p.r#type) & supported_modes == 0 {
            continue;
        }
        // SAFETY: `prop` is valid and `p.name` points to null-terminated C string constant.
        let ret = unsafe { drm_property_add_enum(prop, p.r#type as u64, p.name) };
        if ret != 0 {
            // SAFETY: `dev` and `prop` are valid; destroys property on error.
            unsafe { drm_property_destroy(dev, prop) };
            return ret;
        }
    }

    // SAFETY: `plane->base` is a valid DrmModeObject and `prop` is a valid non-null property.
    unsafe { drm_object_attach_property(&mut (*plane).base, prop, DRM_MODE_BLEND_PREMULTI as u64) };
    // SAFETY: `plane` is a valid pointer to DrmPlane.
    unsafe { (*plane).blend_mode_property = prop };
    0
}

// upstream: drm_blend.c drm_crtc_attach_background_color_property()
#[unsafe(no_mangle)]
/// # Safety
/// `crtc` must point to an initialized `DrmCrtc` with a valid, live `dev` pointer.
/// `crtc->dev->mode_config.background_color_property` must have been created.
/// Caller must ensure exclusive access or synchronization under modeset configuration locks.
pub unsafe extern "C" fn drm_crtc_attach_background_color_property(crtc: *mut DrmCrtc) {
    // SAFETY: `crtc` is a valid, aligned, non-null pointer to DrmCrtc with valid `dev`.
    let crtc = unsafe { &mut *crtc };
    // SAFETY: `crtc.dev` is valid with initialized `mode_config`.
    let prop = unsafe { (*crtc.dev).mode_config.background_color_property };
    // DRM_ARGB64_PREP(0xffff, 0, 0, 0): __DRM_ARGB64_PREP(alpha, 48) | red/green/blue (zero)
    let value: u64 = 0xffffu64 << 48;
    // SAFETY: `crtc.base` is a valid DrmModeObject and `prop` is attached with default opaque black.
    unsafe { drm_object_attach_property(&mut crtc.base as *mut DrmModeObject, prop, value) };
}
