// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_atomic.c`.

#![allow(non_snake_case)]

use alloc::format;
use core::{ffi::c_void, mem::size_of};

use drm_kapi::{
    config::{
        EINVAL, ENOMEM, ENOSPC, EOPNOTSUPP, ERANGE, ETIMEDOUT, GFP_KERNEL, HZ, IS_ERR, PTR_ERR,
    },
    list::{list_add_tail, list_del},
    memory::{kfree, kref_init, kzalloc, kzalloc_objs},
    types::Kref,
};

use crate::{
    drm_atomic_types::{
        DrmAtomicCommit, DrmColoropsState, DrmConnnectorsState, DrmCrtcCommit, DrmCrtcsState,
        DrmPlanesState, DrmPrivateObj, DrmPrivateObjsState, DrmPrivateState, DrmPrivateStateFuncs,
    },
    drm_connector_types::{
        DRM_LINK_STATUS_GOOD, DrmConnector, DrmConnectorListIter, DrmConnectorState,
    },
    drm_crtc_types::{DrmCrtc, DrmCrtcState, DrmModeSet},
    drm_device_types::DrmDevice,
    drm_encoder_types::DrmEncoder,
    drm_mode_object_upstream::DRIVER_ATOMIC,
    drm_plane_types::{DrmPlane, DrmPlaneState},
    drm_print_types::DrmPrinter,
    uapi_drm_consts::*,
};

// upstream: include/drm/drm_atomic.h drm_atomic_crtc_needs_modeset()
/// Checks whether the CRTC state requires a full modeset.
///
/// # Safety
///
/// `state` must point to a valid, initialized `DrmCrtcState` whose memory remains
/// allocated, aligned, and unmodified by concurrent writers for the duration of this call.
#[inline]
pub unsafe fn drm_atomic_crtc_needs_modeset(state: *const DrmCrtcState) -> bool {
    (unsafe {
        // SAFETY: `state` points to a valid, aligned `DrmCrtcState` per caller preconditions;
        // reading mode_changed flag does not mutate state.
        (*state).mode_changed()
    }) || (unsafe {
        // SAFETY: `state` points to a valid, aligned `DrmCrtcState` per caller preconditions;
        // reading active_changed flag does not mutate state.
        (*state).active_changed()
    }) || (unsafe {
        // SAFETY: `state` points to a valid, aligned `DrmCrtcState` per caller preconditions;
        // reading connectors_changed flag does not mutate state.
        (*state).connectors_changed()
    })
}

// # Safety
//
// External C declarations for DRM core runtime functions.
// Callers must satisfy individual C API preconditions:
// - `drm_dev_get` / `drm_dev_put`: `dev` must point to a valid `DrmDevice`.
// - `drm_modeset_lock` / `drm_modeset_unlock`: `lock` must point to an initialized `DrmModesetLock`.
// - `drm_connector_list_iter_*`: pointers must reference valid, initialized iterator structures.
// - `krealloc`: `p` must be null or an existing kernel heap allocation from the same allocator.
unsafe extern "C" {
    fn drm_dev_get(dev: *mut DrmDevice) -> *mut DrmDevice;
    fn drm_dev_put(dev: *mut DrmDevice);
    fn drm_modeset_lock(
        lock: *mut crate::drm_modeset_lock_types::DrmModesetLock,
        ctx: *mut crate::drm_modeset_lock_types::DrmModesetAcquireCtx,
    ) -> i32;
    fn drm_modeset_unlock(lock: *mut crate::drm_modeset_lock_types::DrmModesetLock);
    fn drm_connector_list_iter_begin(dev: *mut DrmDevice, iter: *mut DrmConnectorListIter);
    fn drm_connector_list_iter_next(iter: *mut DrmConnectorListIter) -> *mut DrmConnector;
    fn drm_connector_list_iter_end(iter: *mut DrmConnectorListIter);
    fn krealloc(p: *mut c_void, new_size: usize, flags: u32) -> *mut c_void;
}

/// Returns the index of the given CRTC.
///
/// # Safety
///
/// `crtc` must be a valid, aligned pointer to an initialized `DrmCrtc`
/// whose `index` field is immutable and readable during this call.
#[inline]
unsafe fn drm_crtc_index(crtc: *const DrmCrtc) -> usize {
    unsafe {
        // SAFETY: `crtc` points to an initialized `DrmCrtc` per caller preconditions;
        // the index field is immutable once registered.
        (*crtc).index as usize
    }
}

/// Returns the index of the given plane.
///
/// # Safety
///
/// `plane` must be a valid, aligned pointer to an initialized `DrmPlane`
/// whose `index` field is immutable and readable during this call.
#[inline]
unsafe fn drm_plane_index(plane: *const DrmPlane) -> usize {
    unsafe {
        // SAFETY: `plane` points to an initialized `DrmPlane` per caller preconditions;
        // the index field is immutable once registered.
        (*plane).index as usize
    }
}

/// Returns the index of the given connector.
///
/// # Safety
///
/// `connector` must be a valid, aligned pointer to an initialized `DrmConnector`
/// whose `index` field is immutable and readable during this call.
#[inline]
unsafe fn drm_connector_index(connector: *const DrmConnector) -> usize {
    unsafe {
        // SAFETY: `connector` points to an initialized `DrmConnector` per caller preconditions;
        // the index field is immutable once registered.
        (*connector).index as usize
    }
}

/// Returns a bitmask representing the given CRTC.
///
/// # Safety
///
/// `crtc` must be a valid, aligned pointer to an initialized `DrmCrtc`
/// whose `index` is less than 32 to avoid bitshift overflow.
#[inline]
unsafe fn drm_crtc_mask(crtc: *const DrmCrtc) -> u32 {
    // SAFETY: `crtc` is a valid, aligned pointer per the `# Safety` contract, so reading its `index` is in bounds.
    1u32 << unsafe { (*crtc).index }
}

/// Returns a bitmask representing the given connector.
///
/// # Safety
///
/// `connector` must be a valid, aligned pointer to an initialized `DrmConnector`
/// whose `index` is less than 32 to avoid bitshift overflow.
#[inline]
unsafe fn drm_connector_mask(connector: *const DrmConnector) -> u32 {
    // SAFETY: `connector` is a valid, aligned pointer per the `# Safety` contract, so reading its `index` is in bounds.
    1u32 << unsafe { (*connector).index }
}

/// Acquires a reference to the connector's embedded base mode object if non-null.
///
/// # Safety
///
/// If non-null, `connector` must point to an initialized `DrmConnector` whose embedded
/// `base` mode object has an active refcount.
#[inline]
unsafe fn drm_connector_get(connector: *mut DrmConnector) {
    if !connector.is_null() {
        unsafe {
            // SAFETY: `connector` is verified non-null above, pointing to an initialized `DrmConnector`.
            // Taking the address of embedded `(*connector).base` produces a valid `*mut DrmModeObject`.
            crate::drm_mode_object_upstream::drm_mode_object_get(core::ptr::addr_of_mut!(
                (*connector).base
            ))
        };
    }
}

/// Drops a reference to the connector's embedded base mode object if non-null.
///
/// # Safety
///
/// If non-null, `connector` must point to an initialized `DrmConnector` whose embedded
/// `base` mode object has an active reference to drop.
#[inline]
unsafe fn drm_connector_put(connector: *mut DrmConnector) {
    if !connector.is_null() {
        unsafe {
            // SAFETY: `connector` is verified non-null above, pointing to an initialized `DrmConnector`.
            // Taking the address of embedded `(*connector).base` produces a valid `*mut DrmModeObject`.
            crate::drm_mode_object_upstream::drm_mode_object_put(core::ptr::addr_of_mut!(
                (*connector).base
            ))
        };
    }
}

/// Drops a reference to the given CRTC commit if non-null.
///
/// # Safety
///
/// If non-null, `commit` must point to an initialized `DrmCrtcCommit` whose embedded
/// `r#ref` `Kref` was initialized and currently holds an active reference.
#[inline]
unsafe fn drm_crtc_commit_put(commit: *mut DrmCrtcCommit) {
    if !commit.is_null() {
        unsafe {
            // SAFETY: `commit` was verified non-null above; its embedded `r#ref` is a valid
            // initialized `Kref`, and `__drm_crtc_commit_free` will deallocate the container at 0.
            drm_kapi::memory::kref_put(
                core::ptr::addr_of_mut!((*commit).r#ref),
                __drm_crtc_commit_free,
            )
        };
    }
}

/// Acquires a reference to the atomic commit / state object if non-null.
///
/// # Safety
///
/// If non-null, `state` must point to an initialized `DrmAtomicCommit` with an active refcount.
#[inline]
pub unsafe fn drm_atomic_commit_get(state: *mut DrmAtomicCommit) {
    if !state.is_null() {
        unsafe {
            // SAFETY: `state` was verified non-null above and per caller preconditions points
            // to a live `DrmAtomicCommit` with an initialized `r#ref` field.
            drm_kapi::memory::kref_get(core::ptr::addr_of_mut!((*state).r#ref));
        }
    }
}

pub use drm_atomic_commit_get as drm_atomic_state_get;

/// Drops a reference to the atomic commit / state object if non-null.
///
/// # Safety
///
/// If non-null, `state` must point to an initialized `DrmAtomicCommit` with an active refcount.
#[inline]
pub unsafe fn drm_atomic_commit_put(state: *mut DrmAtomicCommit) {
    if !state.is_null() {
        unsafe {
            // SAFETY: `state` was verified non-null above; its embedded `r#ref` is a valid `Kref`,
            // and `__drm_atomic_commit_free` will clean up and free `state` when refcount drops to 0.
            drm_kapi::memory::kref_put(
                core::ptr::addr_of_mut!((*state).r#ref),
                __drm_atomic_commit_free,
            )
        };
    }
}

pub use drm_atomic_commit_put as drm_atomic_state_put;

/// Returns the old CRTC state for `crtc` in `state`, or null if not found or invalid.
///
/// # Safety
///
/// - If non-null, `state` must point to an initialized `DrmAtomicCommit` whose `crtcs` array
///   (if allocated) remains valid for the duration of this call.
/// - If non-null, `crtc` must point to an initialized `DrmCrtc` associated with the same device
///   as `state`, and `crtc.index` must be strictly less than the number of allocated CRTC entries.
/// - The caller must ensure no concurrent reallocation or mutation of `(*state).crtcs` occurs.
#[inline]
pub unsafe fn drm_atomic_get_old_crtc_state(
    state: *const DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> *mut DrmCrtcState {
    if state.is_null()
        || crtc.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).crtcs.is_null()
        }
    {
        return core::ptr::null_mut();
    }
    unsafe {
        // SAFETY: `state` and `crtc` are non-null and valid. `(*state).crtcs` is non-null,
        // and per caller preconditions `drm_crtc_index(crtc)` is within the allocated bounds of `crtcs`.
        (*(*state).crtcs.add(drm_crtc_index(crtc))).old_state
    }
}

/// Returns the new CRTC state for `crtc` in `state`, or null if not found or invalid.
///
/// # Safety
///
/// - If non-null, `state` must point to an initialized `DrmAtomicCommit` whose `crtcs` array
///   (if allocated) remains valid for the duration of this call.
/// - If non-null, `crtc` must point to an initialized `DrmCrtc` associated with the same device
///   as `state`, and `crtc.index` must be strictly less than the number of allocated CRTC entries.
/// - The caller must ensure no concurrent reallocation or mutation of `(*state).crtcs` occurs.
#[inline]
pub unsafe fn drm_atomic_get_new_crtc_state(
    state: *const DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> *mut DrmCrtcState {
    if state.is_null()
        || crtc.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).crtcs.is_null()
        }
    {
        return core::ptr::null_mut();
    }
    unsafe {
        // SAFETY: `state` and `crtc` are non-null and valid. `(*state).crtcs` is non-null,
        // and per caller preconditions `drm_crtc_index(crtc)` is within the allocated bounds of `crtcs`.
        (*(*state).crtcs.add(drm_crtc_index(crtc))).new_state
    }
}

/// Returns the old plane state for `plane` in `state`, or null if not found or invalid.
///
/// # Safety
///
/// - If non-null, `state` must point to an initialized `DrmAtomicCommit` whose `planes` array
///   (if allocated) remains valid for the duration of this call.
/// - If non-null, `plane` must point to an initialized `DrmPlane` associated with the same device
///   as `state`, and `plane.index` must be strictly less than the number of allocated plane entries.
/// - The caller must ensure no concurrent reallocation or mutation of `(*state).planes` occurs.
#[inline]
pub unsafe fn drm_atomic_get_old_plane_state(
    state: *const DrmAtomicCommit,
    plane: *mut DrmPlane,
) -> *mut DrmPlaneState {
    if state.is_null()
        || plane.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).planes.is_null()
        }
    {
        return core::ptr::null_mut();
    }
    unsafe {
        // SAFETY: `state` and `plane` are non-null and valid. `(*state).planes` is non-null,
        // and per caller preconditions `drm_plane_index(plane)` is within the allocated bounds of `planes`.
        (*(*state).planes.add(drm_plane_index(plane))).old_state
    }
}

/// Returns the new plane state for `plane` in `state`, or null if not found or invalid.
///
/// # Safety
///
/// - If non-null, `state` must point to an initialized `DrmAtomicCommit` whose `planes` array
///   (if allocated) remains valid for the duration of this call.
/// - If non-null, `plane` must point to an initialized `DrmPlane` associated with the same device
///   as `state`, and `plane.index` must be strictly less than the number of allocated plane entries.
/// - The caller must ensure no concurrent reallocation or mutation of `(*state).planes` occurs.
#[inline]
pub unsafe fn drm_atomic_get_new_plane_state(
    state: *const DrmAtomicCommit,
    plane: *mut DrmPlane,
) -> *mut DrmPlaneState {
    if state.is_null()
        || plane.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).planes.is_null()
        }
    {
        return core::ptr::null_mut();
    }
    unsafe {
        // SAFETY: `state` and `plane` are non-null and valid. `(*state).planes` is non-null,
        // and per caller preconditions `drm_plane_index(plane)` is within the allocated bounds of `planes`.
        (*(*state).planes.add(drm_plane_index(plane))).new_state
    }
}

/// Returns the old connector state for `connector` in `state`, or null if not found or invalid.
///
/// # Safety
///
/// - If non-null, `state` must point to an initialized `DrmAtomicCommit`.
/// - If non-null, `connector` must point to an initialized `DrmConnector`.
/// - The caller must ensure no concurrent reallocation or mutation of `(*state).connectors` occurs.
#[inline]
pub unsafe fn drm_atomic_get_old_connector_state(
    state: *const DrmAtomicCommit,
    connector: *mut DrmConnector,
) -> *mut DrmConnectorState {
    if state.is_null()
        || connector.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).connectors.is_null()
        }
    {
        return core::ptr::null_mut();
    }
    let index = unsafe {
        // SAFETY: `connector` was verified non-null above and points to an initialized `DrmConnector`.
        drm_connector_index(connector)
    };
    if index
        >= unsafe {
            // SAFETY: `state` was verified non-null above and points to an initialized `DrmAtomicCommit`.
            (*state).num_connector as usize
        }
    {
        return core::ptr::null_mut();
    }
    unsafe {
        // SAFETY: `(*state).connectors` is non-null, and `index` was verified to be strictly less
        // than `(*state).num_connector`, so `connectors.add(index)` is within allocated bounds.
        (*(*state).connectors.add(index)).old_state
    }
}

/// Returns the new connector state for `connector` in `state`, or null if not found or invalid.
///
/// # Safety
///
/// - If non-null, `state` must point to an initialized `DrmAtomicCommit`.
/// - If non-null, `connector` must point to an initialized `DrmConnector`.
/// - The caller must ensure no concurrent reallocation or mutation of `(*state).connectors` occurs.
#[inline]
pub unsafe fn drm_atomic_get_new_connector_state(
    state: *const DrmAtomicCommit,
    connector: *mut DrmConnector,
) -> *mut DrmConnectorState {
    if state.is_null()
        || connector.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).connectors.is_null()
        }
    {
        return core::ptr::null_mut();
    }
    let index = unsafe {
        // SAFETY: `connector` was verified non-null above and points to an initialized `DrmConnector`.
        drm_connector_index(connector)
    };
    if index
        >= unsafe {
            // SAFETY: `state` was verified non-null above and points to an initialized `DrmAtomicCommit`.
            (*state).num_connector as usize
        }
    {
        return core::ptr::null_mut();
    }
    unsafe {
        // SAFETY: `(*state).connectors` is non-null, and `index` was verified to be strictly less
        // than `(*state).num_connector`, so `connectors.add(index)` is within allocated bounds.
        (*(*state).connectors.add(index)).new_state
    }
}

#[inline]
fn drm_rotation_90_or_270(rotation: u32) -> bool {
    (rotation & (DRM_MODE_ROTATE_90 | DRM_MODE_ROTATE_270)) != 0
}

/// Checks whether the device's driver specifies a feature flag.
///
/// # Safety
///
/// If non-null, `dev` must point to an initialized `DrmDevice`. If `(*dev).driver` is non-null,
/// it must point to an initialized `DrmDriver` readable for the duration of this call.
#[inline]
unsafe fn drm_core_check_feature(dev: *const DrmDevice, feature: u32) -> bool {
    if dev.is_null() {
        return false;
    }
    let drv = unsafe {
        // SAFETY: `dev` was verified non-null above, pointing to an initialized `DrmDevice`.
        (*dev).driver
    };
    if drv.is_null() {
        return false;
    }
    (unsafe {
        // SAFETY: `drv` was verified non-null above, pointing to an initialized `DrmDriver`.
        (*drv).driver_features & feature
    }) != 0
}

/// Checks whether the device driver uses atomic modesetting.
///
/// # Safety
///
/// If non-null, `dev` must point to an initialized `DrmDevice`.
#[inline]
unsafe fn drm_drv_uses_atomic_modeset(dev: *const DrmDevice) -> bool {
    unsafe {
        // SAFETY: `dev` is passed to `drm_core_check_feature` which safely handles null checks
        // and reads feature bits per caller preconditions.
        drm_core_check_feature(dev, DRIVER_ATOMIC)
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c __drm_crtc_commit_free()
/// Destructor callback invoked when a CRTC commit reference count reaches 0.
///
/// # Safety
///
/// `kref` must point to the embedded `r#ref` field of a heap-allocated `DrmCrtcCommit`
/// originally allocated via `kzalloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_crtc_commit_free(kref: *mut Kref) {
    if kref.is_null() {
        return;
    }
    let commit = unsafe {
        // SAFETY: `kref` was verified non-null above and per caller preconditions points to
        // the embedded `r#ref` member of a heap-allocated `DrmCrtcCommit`.
        // Subtracting member offset yields the container pointer.
        (kref as *mut u8).sub(core::mem::offset_of!(DrmCrtcCommit, r#ref))
    } as *mut DrmCrtcCommit;
    unsafe {
        // SAFETY: `commit` was reconstructed from `kref` at final release; freeing it via `kfree`
        // releases the heap memory allocated by `kzalloc`.
        kfree(commit)
    };
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_crtc_commit_wait()
/// Waits for the hardware commit and flip completion on a CRTC commit with a 10s timeout.
///
/// # Safety
///
/// If non-null, `commit` must point to an initialized, live `DrmCrtcCommit` whose completion
/// primitives (`hw_done`, `flip_done`) remain valid and are not deallocated during the wait.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_crtc_commit_wait(commit: *mut DrmCrtcCommit) -> i32 {
    let timeout = 10 * HZ as i64;
    if commit.is_null() {
        return 0;
    }

    // SAFETY: `commit` was checked non-null above and points to an initialized `DrmCrtcCommit`
    // with valid `hw_done` completion primitive.
    let ret = unsafe { (*commit).hw_done.wait_timeout(timeout) };
    if ret == 0 {
        return -ETIMEDOUT;
    }

    // SAFETY: `commit` was checked non-null above and points to an initialized `DrmCrtcCommit`
    // with valid `flip_done` completion primitive.
    let ret = unsafe { (*commit).flip_done.wait_timeout(timeout) };
    if ret == 0 {
        return -ETIMEDOUT;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_commit_default_release()
/// Releases the per-object state pointer arrays associated with an atomic commit.
///
/// # Safety
///
/// If non-null, `state` must point to an initialized `DrmAtomicCommit` whose array pointers
/// (`connectors`, `crtcs`, `planes`, `colorops`, `private_objs`), if non-null, were allocated
/// by the kernel allocator and are not aliased or concurrently accessed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_commit_default_release(state: *mut DrmAtomicCommit) {
    if state.is_null() {
        return;
    }
    unsafe {
        // SAFETY: `state` was verified non-null above. The arrays were allocated via `kzalloc_objs`
        // or `krealloc`. `kfree` safely handles null or valid heap pointers.
        kfree((*state).connectors);
        kfree((*state).crtcs);
        kfree((*state).planes);
        kfree((*state).colorops);
        kfree((*state).private_objs);
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_commit_init()
/// Initializes an uninitialized or zeroed `DrmAtomicCommit` structure.
///
/// # Safety
///
/// - `dev` must point to a valid, initialized `DrmDevice` with an initialized `mode_config`.
/// - `state` must point to an allocated, properly aligned memory block of at least
///   `size_of::<DrmAtomicCommit>()` bytes, unaliased and writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_commit_init(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    if dev.is_null() || state.is_null() {
        return -EINVAL;
    }
    unsafe {
        // SAFETY: `state` is non-null, properly aligned, and unaliased per caller preconditions.
        kref_init(core::ptr::addr_of_mut!((*state).r#ref));
        (*state).set_allow_modeset(true);

        let num_crtc = (*dev).mode_config.num_crtc;
        if num_crtc > 0 {
            (*state).crtcs = kzalloc_objs::<DrmCrtcsState, i32>(num_crtc);
            if (*state).crtcs.is_null() {
                drm_atomic_commit_default_release(state);
                return -ENOMEM;
            }
        }

        let num_planes = (*dev).mode_config.num_total_plane;
        if num_planes > 0 {
            (*state).planes = kzalloc_objs::<DrmPlanesState, i32>(num_planes);
            if (*state).planes.is_null() {
                drm_atomic_commit_default_release(state);
                return -ENOMEM;
            }
        }

        let num_colorops = (*dev).mode_config.num_colorop;
        if num_colorops > 0 {
            (*state).colorops = kzalloc_objs::<DrmColoropsState, i32>(num_colorops);
            if (*state).colorops.is_null() {
                drm_atomic_commit_default_release(state);
                return -ENOMEM;
            }
        }

        drm_dev_get(dev);
        (*state).dev = dev;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_commit_alloc()
/// Allocates and initializes a new `DrmAtomicCommit` structure for `dev`.
///
/// # Safety
///
/// `dev` must point to a valid, initialized `DrmDevice` with an initialized `mode_config`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_commit_alloc(dev: *mut DrmDevice) -> *mut DrmAtomicCommit {
    if dev.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `dev` was verified non-null above, pointing to an initialized `DrmDevice`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
    // SAFETY: `config` is a valid pointer to `dev.mode_config`.
    let funcs = unsafe { (*config).funcs };

    // SAFETY: `funcs` is checked non-null or its `atomic_state_alloc` hook checked.
    if funcs.is_null() || unsafe { (*funcs).atomic_state_alloc.is_none() } {
        let state = kzalloc(size_of::<DrmAtomicCommit>(), GFP_KERNEL) as *mut DrmAtomicCommit;
        if state.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: `state` was just allocated non-null and unaliased from `kzalloc`, and `dev` is valid.
        if unsafe { drm_atomic_commit_init(dev, state) } < 0 {
            unsafe {
                // SAFETY: Initialization failed; free the allocated memory.
                kfree(state);
            }
            return core::ptr::null_mut();
        }
        return state;
    }

    // SAFETY: `funcs` is non-null, `atomic_state_alloc` is verified `Some`, and `dev` is valid.
    unsafe { ((*funcs).atomic_state_alloc.unwrap())(dev) }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_commit_default_clear()
/// Clears all tracked object states and destroys duplicated states in an atomic commit.
///
/// # Safety
///
/// `state` must point to an initialized `DrmAtomicCommit` with valid `dev` and state arrays.
/// The caller must ensure no concurrent readers or writers access `state` during clearing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_commit_default_clear(state: *mut DrmAtomicCommit) {
    if state.is_null() {
        return;
    }
    // SAFETY: `state` is verified non-null above and points to an initialized `DrmAtomicCommit`.
    let dev = unsafe { (*state).dev };
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is verified non-null above and points to an initialized `DrmDevice`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    unsafe {
        // SAFETY: `state`, `dev`, and `config` are non-null and exclusively owned by the caller.
        // We iterate through valid allocated entries and invoke corresponding `atomic_destroy_state`
        // function pointers, dropping references via `drm_connector_put` and `drm_crtc_commit_put`.
        (*state).set_checked(false);

        let num_conn = (*state).num_connector as usize;
        for i in 0..num_conn {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;
            if connector.is_null() {
                continue;
            }

            let funcs = (*connector).funcs;
            if !funcs.is_null()
                && let Some(destroy_fn) = (*funcs).atomic_destroy_state
            {
                destroy_fn(connector, (*conn_entry).state_to_destroy);
            }

            (*conn_entry).ptr = core::ptr::null_mut();
            (*conn_entry).state_to_destroy = core::ptr::null_mut();
            (*conn_entry).old_state = core::ptr::null_mut();
            (*conn_entry).new_state = core::ptr::null_mut();
            drm_connector_put(connector);
        }

        let num_crtc = (*config).num_crtc as usize;
        for i in 0..num_crtc {
            if (*state).crtcs.is_null() {
                break;
            }
            let crtc_entry = (*state).crtcs.add(i);
            let crtc = (*crtc_entry).ptr;
            if crtc.is_null() {
                continue;
            }

            let funcs = (*crtc).funcs;
            if !funcs.is_null()
                && let Some(destroy_fn) = (*funcs).atomic_destroy_state
            {
                destroy_fn(crtc, (*crtc_entry).state_to_destroy);
            }

            (*crtc_entry).ptr = core::ptr::null_mut();
            (*crtc_entry).state_to_destroy = core::ptr::null_mut();
            (*crtc_entry).old_state = core::ptr::null_mut();
            (*crtc_entry).new_state = core::ptr::null_mut();

            if !(*crtc_entry).commit.is_null() {
                drm_crtc_commit_put((*crtc_entry).commit);
                (*crtc_entry).commit = core::ptr::null_mut();
            }
        }

        let num_planes = (*config).num_total_plane as usize;
        for i in 0..num_planes {
            if (*state).planes.is_null() {
                break;
            }
            let plane_entry = (*state).planes.add(i);
            let plane = (*plane_entry).ptr;
            if plane.is_null() {
                continue;
            }

            let funcs = (*plane).funcs;
            if !funcs.is_null()
                && let Some(destroy_fn) = (*funcs).atomic_destroy_state
            {
                destroy_fn(plane, (*plane_entry).state_to_destroy);
            }

            (*plane_entry).ptr = core::ptr::null_mut();
            (*plane_entry).state_to_destroy = core::ptr::null_mut();
            (*plane_entry).old_state = core::ptr::null_mut();
            (*plane_entry).new_state = core::ptr::null_mut();
        }

        let num_priv = (*state).num_private_objs as usize;
        for i in 0..num_priv {
            if (*state).private_objs.is_null() {
                break;
            }
            let priv_entry = (*state).private_objs.add(i);
            let obj = (*priv_entry).ptr;
            if obj.is_null() {
                continue;
            }

            let funcs = (*obj).funcs;
            if !funcs.is_null()
                && let Some(destroy_fn) = (*funcs).atomic_destroy_state
            {
                destroy_fn(obj, (*priv_entry).state_to_destroy);
            }

            (*priv_entry).ptr = core::ptr::null_mut();
            (*priv_entry).state_to_destroy = core::ptr::null_mut();
            (*priv_entry).old_state = core::ptr::null_mut();
            (*priv_entry).new_state = core::ptr::null_mut();
        }
        (*state).num_private_objs = 0;

        if !(*state).fake_commit.is_null() {
            drm_crtc_commit_put((*state).fake_commit);
            (*state).fake_commit = core::ptr::null_mut();
        }
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_commit_clear()
/// Clears the atomic commit, calling the driver's `atomic_state_clear` hook if provided.
///
/// # Safety
///
/// `state` must point to an initialized `DrmAtomicCommit`. The caller must ensure no concurrent
/// access during the clear operation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_commit_clear(state: *mut DrmAtomicCommit) {
    if state.is_null() {
        return;
    }
    // SAFETY: `state` is checked non-null above.
    let dev = unsafe { (*state).dev };
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is verified non-null above.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
    // SAFETY: `config` is a valid pointer to `dev.mode_config`.
    let funcs = unsafe { (*config).funcs };

    if !funcs.is_null()
        // SAFETY: `funcs` is verified non-null.
        && let Some(clear_fn) = unsafe { (*funcs).atomic_state_clear }
    {
        unsafe {
            // SAFETY: `clear_fn` is a valid function pointer from driver `funcs`, and `state` is valid.
            clear_fn(state);
        }
        return;
    }

    unsafe {
        // SAFETY: Fall back to default clearing for the valid `state`.
        drm_atomic_commit_default_clear(state);
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c __drm_atomic_commit_free()
/// Destructor callback invoked when a `DrmAtomicCommit` reference count drops to 0.
///
/// # Safety
///
/// `ref_` must point to the embedded `r#ref` member of a heap-allocated `DrmAtomicCommit`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_commit_free(ref_: *mut Kref) {
    if ref_.is_null() {
        return;
    }
    let state = unsafe {
        // SAFETY: `ref_` is verified non-null above and per caller preconditions points to the embedded
        // `r#ref` of a heap-allocated `DrmAtomicCommit`. Subtracting the member offset yields the container.
        (ref_ as *mut u8).sub(core::mem::offset_of!(DrmAtomicCommit, r#ref))
    } as *mut DrmAtomicCommit;
    let dev = unsafe {
        // SAFETY: `state` was reconstructed from `ref_` and is an initialized `DrmAtomicCommit`.
        (*state).dev
    };
    if dev.is_null() {
        return;
    }
    let config = unsafe {
        // SAFETY: `dev` is verified non-null above, pointing to an initialized `DrmDevice`.
        core::ptr::addr_of_mut!((*dev).mode_config)
    };

    unsafe {
        // SAFETY: `state` reached refcount zero; clearing tracked object states before deallocation.
        drm_atomic_commit_clear(state)
    };

    let funcs = unsafe {
        // SAFETY: `config` points to `dev.mode_config`.
        (*config).funcs
    };
    if !funcs.is_null()
        && unsafe {
            // SAFETY: `funcs` is verified non-null.
            (*funcs).atomic_state_free.is_some()
        }
    {
        unsafe {
            // SAFETY: Driver-specific deallocation hook is non-null; `state` is passed for release.
            ((*funcs).atomic_state_free.unwrap())(state)
        };
    } else {
        unsafe {
            // SAFETY: Default release frees embedded pointer arrays for `state`.
            drm_atomic_commit_default_release(state)
        };
        unsafe {
            // SAFETY: `state` was allocated from kernel heap (`kzalloc`) and its arrays are freed.
            kfree(state)
        };
    }

    unsafe {
        // SAFETY: Drop the device reference that was acquired in `drm_atomic_commit_init`.
        drm_dev_put(dev)
    };
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_crtc_state()
/// Retrieves or duplicates the CRTC state for `crtc` in `state`, acquiring necessary modeset locks.
///
/// # Safety
///
/// - `state` must point to an initialized `DrmAtomicCommit` with valid `acquire_ctx` and `crtcs` array.
/// - `crtc` must point to a live `DrmCrtc` with initialized `mutex` and `funcs`.
/// - `crtc.index` must be within the bounds of `state.crtcs` (allocated to `dev.mode_config.num_crtc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_crtc_state(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> *mut DrmCrtcState {
    if state.is_null() || crtc.is_null() {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }

    // SAFETY: `crtc` is verified non-null above and points to a valid `DrmCrtc`.
    let index = unsafe { drm_crtc_index(crtc) };
    // SAFETY: `state` and `crtc` are non-null and valid per caller preconditions.
    let crtc_state = unsafe { drm_atomic_get_new_crtc_state(state, crtc) };
    if !crtc_state.is_null() {
        return crtc_state;
    }

    // SAFETY: `crtc.mutex` is initialized and `state.acquire_ctx` is a valid acquire context.
    let ret =
        unsafe { drm_modeset_lock(core::ptr::addr_of_mut!((*crtc).mutex), (*state).acquire_ctx) };
    if ret != 0 {
        return drm_kapi::config::ERR_PTR(ret);
    }

    // SAFETY: `crtc` is valid and its `funcs` pointer is accessible.
    let funcs = unsafe { (*crtc).funcs };
    // SAFETY: `funcs` was checked non-null.
    if funcs.is_null() || unsafe { (*funcs).atomic_duplicate_state.is_none() } {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }

    // SAFETY: `atomic_duplicate_state` is non-null and `crtc` is a valid `DrmCrtc` pointer.
    let crtc_state = unsafe { ((*funcs).atomic_duplicate_state.unwrap())(crtc) };
    if crtc_state.is_null() {
        return drm_kapi::config::ERR_PTR(-ENOMEM);
    }

    unsafe {
        // SAFETY: `index` is within bounds of `(*state).crtcs`, and `crtc_state` was just
        // successfully duplicated. The modeset lock is held to protect state updates.
        let entry = (*state).crtcs.add(index);
        (*entry).state_to_destroy = crtc_state;
        (*entry).old_state = (*crtc).state;
        (*entry).new_state = crtc_state;
        (*entry).ptr = crtc;
        (*crtc_state).state = state;
    }

    crtc_state
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_crtc_check()
/// Validates sanity of CRTC state transitions.
///
/// # Safety
///
/// Both `old_crtc_state` and `new_crtc_state` must point to valid, initialized `DrmCrtcState`
/// structures that remain readable during the check. `new_crtc_state.crtc` must point to a live `DrmCrtc`.
unsafe fn drm_atomic_crtc_check(
    old_crtc_state: *const DrmCrtcState,
    new_crtc_state: *const DrmCrtcState,
) -> i32 {
    // SAFETY: `new_crtc_state` is guaranteed valid and readable per caller preconditions.
    let crtc = unsafe { (*new_crtc_state).crtc };
    // SAFETY: `crtc` is guaranteed valid per caller preconditions.
    let dev = unsafe { (*crtc).dev };

    // SAFETY: `new_crtc_state` is valid and readable per caller preconditions.
    if unsafe { (*new_crtc_state).active && !(*new_crtc_state).enable } {
        return -EINVAL;
    }

    // SAFETY: `dev` is a valid `DrmDevice` pointer per caller preconditions.
    if unsafe { drm_core_check_feature(dev, DRIVER_ATOMIC) } {
        // SAFETY: `new_crtc_state` is valid and readable per caller preconditions.
        if unsafe { (*new_crtc_state).enable && (*new_crtc_state).mode_blob.is_null() } {
            return -EINVAL;
        }
        // SAFETY: `new_crtc_state` is valid and readable per caller preconditions.
        if unsafe { !(*new_crtc_state).enable && !(*new_crtc_state).mode_blob.is_null() } {
            return -EINVAL;
        }
    }

    if unsafe {
        // SAFETY: `new_crtc_state` and `old_crtc_state` are valid and readable per caller preconditions.
        !(*new_crtc_state).event.is_null() && !(*new_crtc_state).active && !(*old_crtc_state).active
    } {
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_crtc_print_state()
/// Prints human-readable details of a CRTC state to the given printer.
///
/// # Safety
///
/// If non-null, `p` must point to a valid `DrmPrinter`.
/// If non-null, `state` must point to a valid, initialized `DrmCrtcState`.
unsafe fn drm_atomic_crtc_print_state(p: *mut DrmPrinter, state: *const DrmCrtcState) {
    if p.is_null() || state.is_null() {
        return;
    }
    // SAFETY: `state` was verified non-null above and points to a valid `DrmCrtcState`.
    let crtc = unsafe { (*state).crtc };
    if crtc.is_null() {
        return;
    }

    // SAFETY: `crtc` was null-checked above and points to a valid `DrmCrtc`; reading its `base.id` does not mutate it.
    let line = format!("crtc[{}]: \n", unsafe { (*crtc).base.id });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `crtc id` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `enable` through it only loads a field and does not mutate the state.
    let line = format!("\tenable={}\n", unsafe { (*state).enable as i32 });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `enable` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `active` through it only loads a field and does not mutate the state.
    let line = format!("\tactive={}\n", unsafe { (*state).active as i32 });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `active` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `self_refresh_active` through it only loads a field and does not mutate the state.
    let line = format!("\tself_refresh_active={}\n", unsafe {
        (*state).self_refresh_active as i32
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `self_refresh_active` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `planes_changed` through it only loads a field and does not mutate the state.
    let line = format!("\tplanes_changed={}\n", unsafe {
        (*state).planes_changed() as i32
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `planes_changed` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `mode_changed` through it only loads a field and does not mutate the state.
    let line = format!("\tmode_changed={}\n", unsafe {
        (*state).mode_changed() as i32
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `mode_changed` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `active_changed` through it only loads a field and does not mutate the state.
    let line = format!("\tactive_changed={}\n", unsafe {
        (*state).active_changed() as i32
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `active_changed` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `connectors_changed` through it only loads a field and does not mutate the state.
    let line = format!("\tconnectors_changed={}\n", unsafe {
        (*state).connectors_changed() as i32
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `connectors_changed` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `color_mgmt_changed` through it only loads a field and does not mutate the state.
    let line = format!("\tcolor_mgmt_changed={}\n", unsafe {
        (*state).color_mgmt_changed() as i32
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `color_mgmt_changed` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `plane_mask` through it only loads a field and does not mutate the state.
    let line = format!("\tplane_mask={:x}\n", unsafe { (*state).plane_mask });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `plane_mask` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `connector_mask` through it only loads a field and does not mutate the state.
    let line = format!("\tconnector_mask={:x}\n", unsafe {
        (*state).connector_mask
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `connector_mask` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `encoder_mask` through it only loads a field and does not mutate the state.
    let line = format!("\tencoder_mask={:x}\n", unsafe { (*state).encoder_mask });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `encoder_mask` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };

    // SAFETY: `crtc` was verified non-null above, pointing to an initialized `DrmCrtc`.
    let funcs = unsafe { (*crtc).funcs };
    if !funcs.is_null()
        // SAFETY: `funcs` was verified non-null.
        && let Some(print_fn) = unsafe { (*funcs).atomic_print_state }
    {
        unsafe {
            // SAFETY: `print_fn` is a valid driver callback, `p` is non-null, and `state` is a valid `DrmCrtcState`.
            print_fn(p, state as *mut DrmCrtcState);
        }
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_connector_check()
/// Checks validity of connector state updates (e.g. bpc boundaries, writeback limits).
///
/// # Safety
///
/// `connector` must point to a valid `DrmConnector` and `state` to a valid `DrmConnectorState`.
unsafe fn drm_atomic_connector_check(
    connector: *mut DrmConnector,
    state: *mut DrmConnectorState,
) -> i32 {
    // SAFETY: `connector` points to an initialized `DrmConnector` per caller preconditions.
    let info = unsafe { core::ptr::addr_of!((*connector).display_info) };
    // SAFETY: `info` is a valid pointer to embedded `display_info`.
    let bpc = unsafe { (*info).bpc };
    // SAFETY: `state` points to an initialized `DrmConnectorState` writable by caller.
    unsafe { (*state).max_bpc = if bpc != 0 { bpc as u8 } else { 8 } };

    // SAFETY: `connector` points to an initialized `DrmConnector`.
    if !unsafe { (*connector).max_bpc_property.is_null() } {
        unsafe {
            // SAFETY: `state` points to an initialized `DrmConnectorState` writable by caller.
            (*state).max_bpc = core::cmp::min((*state).max_bpc, (*state).max_requested_bpc);
        }
    }

    // SAFETY: `connector` points to an initialized `DrmConnector`.
    if unsafe { (*connector).connector_type == DRM_MODE_CONNECTOR_WRITEBACK as i32 }
        // SAFETY: `state` points to an initialized `DrmConnectorState`.
        || !unsafe { (*state).writeback_job.is_null() }
    {
        return -EOPNOTSUPP;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_plane_state()
/// Retrieves or duplicates the plane state for `plane` in `state`, acquiring necessary modeset locks.
///
/// # Safety
///
/// - `state` must point to an initialized `DrmAtomicCommit` with valid `acquire_ctx` and `planes` array.
/// - `plane` must point to a live `DrmPlane` with initialized `mutex` and `funcs`.
/// - `plane.index` must be strictly less than the number of allocated planes (`dev.mode_config.num_total_plane`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_plane_state(
    state: *mut DrmAtomicCommit,
    plane: *mut DrmPlane,
) -> *mut DrmPlaneState {
    if state.is_null() || plane.is_null() {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }

    // SAFETY: `plane` was verified non-null above and points to a valid `DrmPlane`.
    let index = unsafe { drm_plane_index(plane) };
    // SAFETY: `state` and `plane` are non-null and valid per caller preconditions.
    let plane_state = unsafe { drm_atomic_get_new_plane_state(state, plane) };
    if !plane_state.is_null() {
        return plane_state;
    }

    let ret = unsafe {
        // SAFETY: `plane.mutex` is initialized and `state.acquire_ctx` is a valid acquire context.
        drm_modeset_lock(
            core::ptr::addr_of_mut!((*plane).mutex),
            (*state).acquire_ctx,
        )
    };
    if ret != 0 {
        return drm_kapi::config::ERR_PTR(ret);
    }

    // SAFETY: `plane` is valid and its `funcs` pointer is accessible.
    let funcs = unsafe { (*plane).funcs };
    // SAFETY: `funcs` was checked non-null.
    if funcs.is_null() || unsafe { (*funcs).atomic_duplicate_state.is_none() } {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }

    // SAFETY: `atomic_duplicate_state` is non-null and `plane` is a valid `DrmPlane` pointer.
    let plane_state = unsafe { ((*funcs).atomic_duplicate_state.unwrap())(plane) };
    if plane_state.is_null() {
        return drm_kapi::config::ERR_PTR(-ENOMEM);
    }

    unsafe {
        // SAFETY: `index` is within bounds of `(*state).planes`, and `plane_state` was just
        // successfully duplicated. The modeset lock is held to protect state updates.
        let entry = (*state).planes.add(index);
        (*entry).state_to_destroy = plane_state;
        (*entry).ptr = plane;
        (*entry).old_state = (*plane).state;
        (*entry).new_state = plane_state;
        (*plane_state).state = state;

        if !(*plane_state).crtc.is_null() {
            let crtc_state = drm_atomic_get_crtc_state(state, (*plane_state).crtc);
            if IS_ERR(crtc_state) {
                return crtc_state.cast();
            }
        }
    }

    plane_state
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_colorop_state()
/// Stub for colorop state retrieval.
///
/// # Safety
///
/// No caller preconditions; always returns null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_colorop_state(
    _state: *mut DrmAtomicCommit,
    _colorop: *mut c_void,
) -> *mut c_void {
    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_old_colorop_state()
/// Stub for old colorop state retrieval.
///
/// # Safety
///
/// No caller preconditions; always returns null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_old_colorop_state(
    _state: *mut DrmAtomicCommit,
    _colorop: *mut c_void,
) -> *mut c_void {
    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_new_colorop_state()
/// Stub for new colorop state retrieval.
///
/// # Safety
///
/// No caller preconditions; always returns null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_new_colorop_state(
    _state: *mut DrmAtomicCommit,
    _colorop: *mut c_void,
) -> *mut c_void {
    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_atomic.c plane_switching_crtc()
/// Checks whether a plane is transitioning between two different non-null CRTCs.
///
/// # Safety
///
/// Both `old_plane_state` and `new_plane_state` must point to valid, initialized `DrmPlaneState`
/// structures readable for the duration of this call.
unsafe fn plane_switching_crtc(
    old_plane_state: *const DrmPlaneState,
    new_plane_state: *const DrmPlaneState,
) -> bool {
    // SAFETY: Both plane states are valid and readable per caller preconditions.
    if unsafe { (*old_plane_state).crtc.is_null() || (*new_plane_state).crtc.is_null() } {
        return false;
    }
    // SAFETY: Both plane states are valid and readable per caller preconditions.
    unsafe { (*old_plane_state).crtc != (*new_plane_state).crtc }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_plane_check()
/// Validates plane state parameters (CRTC association, format/modifier support, coordinates, clipping).
///
/// # Safety
///
/// Both `old_plane_state` and `new_plane_state` must point to valid, initialized `DrmPlaneState`
/// structures that remain readable during the check.
unsafe fn drm_atomic_plane_check(
    old_plane_state: *const DrmPlaneState,
    new_plane_state: *const DrmPlaneState,
) -> i32 {
    let plane = unsafe {
        // SAFETY: `new_plane_state` points to a valid, initialized `DrmPlaneState` per caller preconditions.
        (*new_plane_state).plane
    };
    let crtc = unsafe {
        // SAFETY: `new_plane_state` points to a valid, initialized `DrmPlaneState` per caller preconditions.
        (*new_plane_state).crtc
    };
    let fb = unsafe {
        // SAFETY: `new_plane_state` points to a valid, initialized `DrmPlaneState` per caller preconditions.
        (*new_plane_state).fb
    };

    if crtc.is_null() != fb.is_null() {
        return -EINVAL;
    }

    if crtc.is_null() {
        return 0;
    }

    if (unsafe {
        // SAFETY: `plane` was retrieved from valid `new_plane_state` and points to an initialized `DrmPlane`.
        (*plane).possible_crtcs
    } & unsafe {
        // SAFETY: `crtc` is non-null and points to an initialized `DrmCrtc`.
        drm_crtc_mask(crtc)
    }) == 0
    {
        return -EINVAL;
    }

    let format = unsafe {
        // SAFETY: `fb` is verified non-null and points to an initialized `DrmFramebuffer` with valid `format`.
        (*(*fb).format).format
    };
    let modifier = unsafe {
        // SAFETY: `fb` is verified non-null and points to an initialized `DrmFramebuffer`.
        (*fb).modifier
    };
    if !unsafe {
        // SAFETY: `plane` is a valid `DrmPlane` pointer, and format/modifier are valid parameters.
        crate::drm_plane_upstream::drm_plane_has_format(plane, format, modifier)
    } {
        return -EINVAL;
    }

    let crtc_w = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        (*new_plane_state).crtc_w
    };
    let crtc_h = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        (*new_plane_state).crtc_h
    };
    let crtc_x = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        (*new_plane_state).crtc_x
    };
    let crtc_y = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        (*new_plane_state).crtc_y
    };

    if crtc_w > i32::MAX as u32
        || crtc_x > i32::MAX - crtc_w as i32
        || crtc_h > i32::MAX as u32
        || crtc_y > i32::MAX - crtc_h as i32
    {
        return -ERANGE;
    }

    let fb_width = unsafe {
        // SAFETY: `fb` is verified non-null and points to an initialized `DrmFramebuffer`.
        (*fb).width
    } << 16;
    let fb_height = unsafe {
        // SAFETY: `fb` is verified non-null and points to an initialized `DrmFramebuffer`.
        (*fb).height
    } << 16;
    let src_w = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        (*new_plane_state).src_w
    };
    let src_h = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        (*new_plane_state).src_h
    };
    let src_x = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        (*new_plane_state).src_x
    };
    let src_y = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        (*new_plane_state).src_y
    };

    if src_w > fb_width
        || src_x > fb_width - src_w
        || src_h > fb_height
        || src_y > fb_height - src_h
    {
        return -ENOSPC;
    }

    let mut clips = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        crate::drm_plane_upstream::__drm_plane_get_damage_clips(new_plane_state)
    };
    let mut num_clips = unsafe {
        // SAFETY: `new_plane_state` points to an initialized `DrmPlaneState`.
        crate::drm_plane_upstream::drm_plane_get_damage_clips_count(new_plane_state)
    };

    while num_clips > 0 {
        if unsafe {
            // SAFETY: `clips` points to a valid `DrmRect` within the clip array of length `num_clips`.
            (*clips).x1
        } >= unsafe {
            // SAFETY: `clips` points to a valid `DrmRect`.
            (*clips).x2
        } || unsafe {
            // SAFETY: `clips` points to a valid `DrmRect`.
            (*clips).y1
        } >= unsafe {
            // SAFETY: `clips` points to a valid `DrmRect`.
            (*clips).y2
        } || unsafe {
            // SAFETY: `clips` points to a valid `DrmRect`.
            (*clips).x1
        } < 0
            || unsafe {
                // SAFETY: `clips` points to a valid `DrmRect`.
                (*clips).y1
            } < 0
            || unsafe {
                // SAFETY: `clips` points to a valid `DrmRect`.
                (*clips).x2
            } > fb_width as i32
            || unsafe {
                // SAFETY: `clips` points to a valid `DrmRect`.
                (*clips).y2
            } > fb_height as i32
        {
            return -EINVAL;
        }
        clips = unsafe {
            // SAFETY: `clips` advances to the next `DrmRect` within the allocated array bounds.
            clips.add(1)
        };

        num_clips -= 1;
    }

    if unsafe {
        // SAFETY: Both plane states are valid per caller preconditions.
        plane_switching_crtc(old_plane_state, new_plane_state)
    } {
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_plane_print_state()
/// Prints human-readable details of a plane state to the given printer.
///
/// # Safety
///
/// If non-null, `p` must point to a valid `DrmPrinter`.
/// If non-null, `state` must point to a valid, initialized `DrmPlaneState`.
unsafe fn drm_atomic_plane_print_state(p: *mut DrmPrinter, state: *const DrmPlaneState) {
    if p.is_null() || state.is_null() {
        return;
    }
    // SAFETY: `state` was verified non-null above and points to a valid `DrmPlaneState`.
    let plane = unsafe { (*state).plane };
    if plane.is_null() {
        return;
    }

    // SAFETY: `state` was null-checked on entry; reading `encoder_mask` through it only loads a field and does not mutate the state.
    let line = format!("plane[{}]: \n", unsafe { (*plane).base.id });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `plane` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` points to a valid `DrmPlaneState`.
    let crtc = unsafe { (*state).crtc };
    // SAFETY: `plane` was null-checked above and points to a valid object; reading its `base.id` does not mutate it.
    let line = format!(
        "\tcrtc={}\n",
        if !crtc.is_null() { "crtc" } else { "(null)" }
    );
    // SAFETY: `p` is checked non-null above.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` points to a valid `DrmPlaneState`.
    let fb = unsafe { (*state).fb };
    // SAFETY: `state` was null-checked on entry; reading `crtc` through it only loads a field and does not mutate the state.
    let line = format!(
        "\tfb={}\n",
        if !fb.is_null() {
            // SAFETY: `fb` is non-null and points to an initialized `DrmFramebuffer`.
            unsafe { (*fb).base.id }
        } else {
            0
        }
    );
    // SAFETY: `p` is checked non-null above.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    if !fb.is_null() {
        unsafe {
            // SAFETY: `fb` is non-null and `p` is a valid printer.
            crate::drm_framebuffer_upstream::drm_framebuffer_print_info(p, 2, fb);
        }
    }
    // SAFETY: `state` was null-checked on entry; reading `rotation` through it only loads a field and does not mutate the state.
    let line = format!("\trotation={:x}\n", unsafe { (*state).rotation });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `rotation` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `normalized-zpos` through it only loads a field and does not mutate the state.
    let line = format!("\tnormalized-zpos={:x}\n", unsafe {
        (*state).normalized_zpos
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `normalized-zpos` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` was null-checked on entry; reading `color_mgmt_changed` through it only loads a field and does not mutate the state.
    let line = format!("\tcolor_mgmt_changed={}\n", unsafe {
        (*state).color_mgmt_changed() as i32
    });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `color_mgmt_changed` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };

    // SAFETY: `plane` was verified non-null above.
    let funcs = unsafe { (*plane).funcs };
    if !funcs.is_null()
        // SAFETY: `funcs` was verified non-null.
        && let Some(print_fn) = unsafe { (*funcs).atomic_print_state }
    {
        unsafe {
            // SAFETY: `print_fn` is a valid driver callback, `p` is non-null, and `state` is a valid `DrmPlaneState`.
            print_fn(p, state as *mut DrmPlaneState);
        }
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_private_obj_init()
/// Initializes a private DRM object and registers it in the device's private object list.
///
/// # Safety
///
/// - `dev` must point to an initialized `DrmDevice` with an initialized `mode_config.privobj_list`.
/// - `obj` must point to allocated, unaliased memory suitable for `DrmPrivateObj`.
/// - `funcs` must point to an initialized `DrmPrivateStateFuncs` table that outlives `obj`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_private_obj_init(
    dev: *mut DrmDevice,
    obj: *mut DrmPrivateObj,
    funcs: *const DrmPrivateStateFuncs,
) -> i32 {
    if dev.is_null() || obj.is_null() || funcs.is_null() {
        return -EINVAL;
    }

    unsafe {
        // SAFETY: `obj` is unaliased and writable for `size_of::<DrmPrivateObj>()`.
        core::ptr::write_bytes(obj, 0, 1);
        // SAFETY: `obj.lock` is an unaliased `DrmModesetLock` field being initialized.
        crate::drm_modeset_lock_upstream::drm_modeset_lock_init(core::ptr::addr_of_mut!(
            (*obj).lock
        ));

        (*obj).dev = dev;
        (*obj).funcs = funcs as *mut DrmPrivateStateFuncs;

        // SAFETY: `dev.mode_config.privobj_list` is a valid list head, and `obj.head` is initialized.
        let priv_list = core::ptr::addr_of_mut!((*dev).mode_config.privobj_list);
        list_add_tail(core::ptr::addr_of_mut!((*obj).head), priv_list);

        // SAFETY: `funcs` was checked non-null above.
        if let Some(create_fn) = (*funcs).atomic_create_state {
            let state = create_fn(obj);
            if IS_ERR(state) {
                return PTR_ERR(state);
            }
            (*obj).state = state;
        }
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_private_obj_fini()
/// Finalizes a private DRM object and unlinks it from the device's private object list.
///
/// # Safety
///
/// If non-null, `obj` must point to an initialized `DrmPrivateObj` currently linked in a list.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_private_obj_fini(obj: *mut DrmPrivateObj) {
    if obj.is_null() {
        return;
    }
    unsafe {
        // SAFETY: `obj` was verified non-null above and is linked in `privobj_list`.
        list_del(core::ptr::addr_of_mut!((*obj).head));
        let funcs = (*obj).funcs;
        if !funcs.is_null()
            && let Some(destroy_fn) = (*funcs).atomic_destroy_state
        {
            // SAFETY: Destroy hook is called to deallocate `obj.state`.
            destroy_fn(obj, (*obj).state);
        }
        // drm_modeset_lock_fini is not needed
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_private_obj_state()
/// Retrieves or duplicates the state for a private object in `state`, locking its modeset lock.
///
/// # Safety
///
/// - `state` must point to an initialized `DrmAtomicCommit` with a valid `acquire_ctx`.
/// - `obj` must point to a live `DrmPrivateObj` with initialized `lock` and `funcs`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_private_obj_state(
    state: *mut DrmAtomicCommit,
    obj: *mut DrmPrivateObj,
) -> *mut DrmPrivateState {
    if state.is_null() || obj.is_null() {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }

    // SAFETY: `state` and `obj` are verified non-null above.
    let obj_state = unsafe { drm_atomic_get_new_private_obj_state(state, obj) };
    if !obj_state.is_null() {
        return obj_state;
    }

    let ret =
        // SAFETY: `obj.lock` is an initialized `DrmModesetLock` and `state.acquire_ctx` is valid.
        unsafe { drm_modeset_lock(core::ptr::addr_of_mut!((*obj).lock), (*state).acquire_ctx) };
    if ret != 0 {
        return drm_kapi::config::ERR_PTR(ret);
    }

    // SAFETY: `state` is a valid `DrmAtomicCommit` pointer.
    let num_objs = unsafe { (*state).num_private_objs + 1 } as usize;
    let size = size_of::<DrmPrivateObjsState>() * num_objs;
    let arr = unsafe {
        // SAFETY: `state.private_objs` is either null or an allocation from `krealloc`/`kzalloc`.
        krealloc((*state).private_objs as *mut c_void, size, GFP_KERNEL) as *mut DrmPrivateObjsState
    };
    if arr.is_null() {
        return drm_kapi::config::ERR_PTR(-ENOMEM);
    }

    unsafe {
        // SAFETY: `arr` is a newly reallocated buffer of size `size` with capacity for `num_objs`.
        (*state).private_objs = arr;
        let index = (*state).num_private_objs as usize;
        core::ptr::write_bytes((*state).private_objs.add(index), 0, 1);

        let funcs = (*obj).funcs;
        if funcs.is_null() || (*funcs).atomic_duplicate_state.is_none() {
            return drm_kapi::config::ERR_PTR(-EINVAL);
        }

        let obj_state = ((*funcs).atomic_duplicate_state.unwrap())(obj);
        if obj_state.is_null() {
            return drm_kapi::config::ERR_PTR(-ENOMEM);
        }

        let entry = (*state).private_objs.add(index);
        (*entry).state_to_destroy = obj_state;
        (*entry).old_state = (*obj).state;
        (*entry).new_state = obj_state;
        (*entry).ptr = obj;
        (*obj_state).state = state;

        (*state).num_private_objs = num_objs as i32;

        obj_state
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_old_private_obj_state()
/// Searches `state` for the old private state corresponding to `obj`.
///
/// # Safety
///
/// If non-null, `state` must point to a valid `DrmAtomicCommit`, and `obj` to a valid `DrmPrivateObj`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_old_private_obj_state(
    state: *const DrmAtomicCommit,
    obj: *mut DrmPrivateObj,
) -> *mut DrmPrivateState {
    if state.is_null()
        || obj.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).private_objs.is_null()
        }
    {
        return core::ptr::null_mut();
    }

    for i in 0..(unsafe {
        // SAFETY: `state` was verified non-null above, pointing to an initialized `DrmAtomicCommit`.
        (*state).num_private_objs as usize
    }) {
        if unsafe {
            // SAFETY: `i` is strictly less than `(*state).num_private_objs`, within allocated array bounds.
            (*(*state).private_objs.add(i)).ptr == obj
        } {
            return unsafe {
                // SAFETY: `i` is strictly less than `(*state).num_private_objs`, within allocated array bounds.
                (*(*state).private_objs.add(i)).old_state
            };
        }
    }

    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_new_private_obj_state()
/// Searches `state` for the new private state corresponding to `obj`.
///
/// # Safety
///
/// If non-null, `state` must point to a valid `DrmAtomicCommit`, and `obj` to a valid `DrmPrivateObj`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_new_private_obj_state(
    state: *const DrmAtomicCommit,
    obj: *mut DrmPrivateObj,
) -> *mut DrmPrivateState {
    if state.is_null()
        || obj.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).private_objs.is_null()
        }
    {
        return core::ptr::null_mut();
    }

    for i in 0..(unsafe {
        // SAFETY: `state` was verified non-null above, pointing to an initialized `DrmAtomicCommit`.
        (*state).num_private_objs as usize
    }) {
        if unsafe {
            // SAFETY: `i` is strictly less than `(*state).num_private_objs`, within allocated array bounds.
            (*(*state).private_objs.add(i)).ptr == obj
        } {
            return unsafe {
                // SAFETY: `i` is strictly less than `(*state).num_private_objs`, within allocated array bounds.
                (*(*state).private_objs.add(i)).new_state
            };
        }
    }

    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_old_connector_for_encoder()
/// Finds the connector whose old state was driven by `encoder`.
///
/// # Safety
///
/// If non-null, `state` must point to a valid `DrmAtomicCommit`, and `encoder` to a valid `DrmEncoder`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_old_connector_for_encoder(
    state: *const DrmAtomicCommit,
    encoder: *mut DrmEncoder,
) -> *mut DrmConnector {
    if state.is_null()
        || encoder.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).connectors.is_null()
        }
    {
        return core::ptr::null_mut();
    }

    for i in 0..(unsafe {
        // SAFETY: `state` was verified non-null above, pointing to an initialized `DrmAtomicCommit`.
        (*state).num_connector as usize
    }) {
        let conn = unsafe {
            // SAFETY: `i` is strictly less than `(*state).num_connector`, within allocated bounds.
            (*(*state).connectors.add(i)).ptr
        };
        let conn_state = unsafe {
            // SAFETY: `i` is strictly less than `(*state).num_connector`, within allocated bounds.
            (*(*state).connectors.add(i)).old_state
        };
        if !conn_state.is_null()
            && unsafe {
                // SAFETY: `conn_state` was checked non-null above, pointing to a valid `DrmConnectorState`.
                (*conn_state).best_encoder == encoder
            }
        {
            return conn;
        }
    }

    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_new_connector_for_encoder()
/// Finds the connector whose new state is driven by `encoder`.
///
/// # Safety
///
/// If non-null, `state` must point to a valid `DrmAtomicCommit`, and `encoder` to a valid `DrmEncoder`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_new_connector_for_encoder(
    state: *const DrmAtomicCommit,
    encoder: *mut DrmEncoder,
) -> *mut DrmConnector {
    if state.is_null()
        || encoder.is_null()
        || unsafe {
            // SAFETY: `state` was verified non-null above, pointing to an aligned `DrmAtomicCommit`.
            (*state).connectors.is_null()
        }
    {
        return core::ptr::null_mut();
    }

    for i in 0..(unsafe {
        // SAFETY: `state` was verified non-null above, pointing to an initialized `DrmAtomicCommit`.
        (*state).num_connector as usize
    }) {
        let conn = unsafe {
            // SAFETY: `i` is strictly less than `(*state).num_connector`, within allocated bounds.
            (*(*state).connectors.add(i)).ptr
        };
        let conn_state = unsafe {
            // SAFETY: `i` is strictly less than `(*state).num_connector`, within allocated bounds.
            (*(*state).connectors.add(i)).new_state
        };
        if !conn_state.is_null()
            && unsafe {
                // SAFETY: `conn_state` was checked non-null above, pointing to a valid `DrmConnectorState`.
                (*conn_state).best_encoder == encoder
            }
        {
            return conn;
        }
    }

    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_connector_for_encoder()
/// Iterates over all connectors of the encoder's device to find which connector uses `encoder`.
///
/// # Safety
///
/// `encoder` must point to a live `DrmEncoder` with an initialized `dev` and `mode_config.connection_mutex`.
/// `ctx` must be null or point to a valid `DrmModesetAcquireCtx`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_connector_for_encoder(
    encoder: *const DrmEncoder,
    ctx: *mut crate::drm_modeset_lock_types::DrmModesetAcquireCtx,
) -> *mut DrmConnector {
    if encoder.is_null() {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }
    // SAFETY: `encoder` was verified non-null above and points to a valid `DrmEncoder`.
    let dev = unsafe { (*encoder).dev };
    if dev.is_null() {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }

    // SAFETY: `dev` is non-null and points to an initialized `DrmDevice`.
    let conn_mutex = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex) };
    // SAFETY: `conn_mutex` is a valid pointer to `connection_mutex`, and `ctx` is an acquire context.
    let ret = unsafe { drm_modeset_lock(conn_mutex, ctx) };
    if ret != 0 {
        return drm_kapi::config::ERR_PTR(ret);
    }

    let mut out_connector: *mut DrmConnector = drm_kapi::config::ERR_PTR(-EINVAL);
    let mut conn_iter = core::mem::MaybeUninit::<DrmConnectorListIter>::uninit();

    unsafe {
        // SAFETY: `dev` is valid, and `conn_iter` memory is uninitialized storage for the iterator.
        drm_connector_list_iter_begin(dev, conn_iter.as_mut_ptr());
        let iter = conn_iter.as_mut_ptr();
        loop {
            let connector = drm_connector_list_iter_next(iter);
            if connector.is_null() {
                break;
            }
            if (*connector).state.is_null() {
                continue;
            }
            if core::ptr::eq(encoder, (*(*connector).state).best_encoder) {
                out_connector = connector;
                break;
            }
        }
        drm_connector_list_iter_end(iter);
        drm_modeset_unlock(conn_mutex);
    }

    out_connector
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_old_crtc_for_encoder()
/// Returns the old CRTC that was driving `encoder`, or null.
///
/// # Safety
///
/// `state` must point to a valid `DrmAtomicCommit` and `encoder` to a valid `DrmEncoder`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_old_crtc_for_encoder(
    state: *mut DrmAtomicCommit,
    encoder: *mut DrmEncoder,
) -> *mut DrmCrtc {
    // SAFETY: `state` and `encoder` are valid pointers per caller preconditions.
    let connector = unsafe { drm_atomic_get_old_connector_for_encoder(state, encoder) };
    if connector.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `state` and `connector` are valid pointers.
    let conn_state = unsafe { drm_atomic_get_old_connector_state(state, connector) };
    if conn_state.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `conn_state` was checked non-null above, pointing to a valid `DrmConnectorState`.
    unsafe { (*conn_state).crtc }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_new_crtc_for_encoder()
/// Returns the new CRTC that will drive `encoder`, or null.
///
/// # Safety
///
/// `state` must point to a valid `DrmAtomicCommit` and `encoder` to a valid `DrmEncoder`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_new_crtc_for_encoder(
    state: *mut DrmAtomicCommit,
    encoder: *mut DrmEncoder,
) -> *mut DrmCrtc {
    // SAFETY: `state` and `encoder` are valid pointers per caller preconditions.
    let connector = unsafe { drm_atomic_get_new_connector_for_encoder(state, encoder) };
    if connector.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `state` and `connector` are valid pointers.
    let conn_state = unsafe { drm_atomic_get_new_connector_state(state, connector) };
    if conn_state.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `conn_state` was checked non-null above, pointing to a valid `DrmConnectorState`.
    unsafe { (*conn_state).crtc }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_get_connector_state()
/// Retrieves or duplicates the connector state in `state`, locking `connection_mutex` and reallocating array if needed.
///
/// # Safety
///
/// - `state` must point to an initialized `DrmAtomicCommit`.
/// - `connector` must point to a live `DrmConnector` with initialized `dev` and `funcs`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_connector_state(
    state: *mut DrmAtomicCommit,
    connector: *mut DrmConnector,
) -> *mut DrmConnectorState {
    if state.is_null() || connector.is_null() {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }

    // SAFETY: `connector` was verified non-null above, pointing to a valid `DrmConnector`.
    let dev = unsafe { (*connector).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    let ret = unsafe {
        // SAFETY: `config.connection_mutex` is an initialized modeset lock and `state.acquire_ctx` is valid.
        drm_modeset_lock(
            core::ptr::addr_of_mut!((*config).connection_mutex),
            (*state).acquire_ctx,
        )
    };
    if ret != 0 {
        return drm_kapi::config::ERR_PTR(ret);
    }

    // SAFETY: `connector` is non-null and valid.
    let index = unsafe { drm_connector_index(connector) };
    // SAFETY: `state` is non-null and valid.
    if index >= unsafe { (*state).num_connector as usize } {
        // SAFETY: `config` is a valid pointer to `mode_config`.
        let alloc = core::cmp::max(index + 1, unsafe { (*config).num_connector as usize });
        let size = size_of::<DrmConnnectorsState>() * alloc;
        let c = unsafe {
            // SAFETY: `state.connectors` is null or a live allocation from `krealloc`/`kzalloc`.
            krealloc((*state).connectors as *mut c_void, size, GFP_KERNEL)
                as *mut DrmConnnectorsState
        };
        if c.is_null() {
            return drm_kapi::config::ERR_PTR(-ENOMEM);
        }

        unsafe {
            // SAFETY: `c` is a valid reallocated buffer of size `size`. Zero out newly added elements.
            (*state).connectors = c;
            let old_num = (*state).num_connector as usize;
            core::ptr::write_bytes((*state).connectors.add(old_num), 0, alloc - old_num);
            (*state).num_connector = alloc as i32;
        }
    }

    // SAFETY: `state` and `connector` are valid pointers.
    let connector_state = unsafe { drm_atomic_get_new_connector_state(state, connector) };
    if !connector_state.is_null() {
        return connector_state;
    }

    // SAFETY: `connector` is valid and its `funcs` pointer is accessible.
    let funcs = unsafe { (*connector).funcs };
    // SAFETY: `funcs` was checked non-null.
    if funcs.is_null() || unsafe { (*funcs).atomic_duplicate_state.is_none() } {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }

    // SAFETY: `atomic_duplicate_state` hook is non-null and `connector` is valid.
    let connector_state = unsafe { ((*funcs).atomic_duplicate_state.unwrap())(connector) };
    if connector_state.is_null() {
        return drm_kapi::config::ERR_PTR(-ENOMEM);
    }

    unsafe {
        // SAFETY: `connector` is a live connector whose refcount is bumped by `drm_connector_get`.
        drm_connector_get(connector);
        // SAFETY: `index` was verified < `num_connector` (reallocated if needed above).
        let entry = (*state).connectors.add(index);
        (*entry).state_to_destroy = connector_state;
        (*entry).old_state = (*connector).state;
        (*entry).new_state = connector_state;
        (*entry).ptr = connector;
        (*connector_state).state = state;

        if !(*connector_state).crtc.is_null() {
            let crtc_state = drm_atomic_get_crtc_state(state, (*connector_state).crtc);
            if IS_ERR(crtc_state) {
                return crtc_state.cast();
            }
        }
    }

    connector_state
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_connector_print_state()
/// Prints human-readable details of a connector state to the given printer.
///
/// # Safety
///
/// If non-null, `p` must point to a valid `DrmPrinter`.
/// If non-null, `state` must point to a valid, initialized `DrmConnectorState`.
unsafe fn drm_atomic_connector_print_state(p: *mut DrmPrinter, state: *const DrmConnectorState) {
    if p.is_null() || state.is_null() {
        return;
    }
    // SAFETY: `state` was verified non-null above and points to a valid `DrmConnectorState`.
    let connector = unsafe { (*state).connector };
    if connector.is_null() {
        return;
    }

    // SAFETY: `state` was null-checked on entry; reading `color_mgmt_changed` through it only loads a field and does not mutate the state.
    let line = format!("connector[{}]: \n", unsafe { (*connector).base.id });
    // SAFETY: `p` was null-checked on entry, and `line` is the String formatted from `connector` that stays alive until this call returns.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
    // SAFETY: `state` points to a valid `DrmConnectorState`.
    let crtc = unsafe { (*state).crtc };
    // SAFETY: `connector` was null-checked above and points to a valid object; reading its `base.id` does not mutate it.
    let line = format!(
        "\tcrtc={}\n",
        if !crtc.is_null() { "crtc" } else { "(null)" }
    );
    // SAFETY: `p` is checked non-null above.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };

    // SAFETY: `connector` is non-null.
    let funcs = unsafe { (*connector).funcs };
    if !funcs.is_null()
        // SAFETY: `funcs` is non-null.
        && let Some(print_fn) = unsafe { (*funcs).atomic_print_state }
    {
        unsafe {
            // SAFETY: `print_fn` is a valid driver callback, `p` is non-null, and `state` is a valid `DrmConnectorState`.
            print_fn(p, state as *mut DrmConnectorState);
        }
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_add_encoder_bridges()
/// Adds bridge states associated with `encoder` to `state`.
///
/// # Safety
///
/// If non-null, `encoder` must point to an initialized `DrmEncoder`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_add_encoder_bridges(
    state: *mut DrmAtomicCommit,
    encoder: *mut DrmEncoder,
) -> i32 {
    if state.is_null() || encoder.is_null() {
        return -EINVAL;
    }
    // SAFETY: `encoder` was verified non-null above, pointing to an initialized `DrmEncoder`.
    if !drm_kapi::list::list_empty(unsafe { &(*encoder).bridge_chain }) {
        return -EOPNOTSUPP;
    }
    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_add_affected_connectors()
/// Adds all connectors routed to `crtc` to the atomic commit.
///
/// # Safety
///
/// - `state` must point to an initialized `DrmAtomicCommit`.
/// - `crtc` must point to a live `DrmCrtc` belonging to the same device as `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_add_affected_connectors(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> i32 {
    if state.is_null() || crtc.is_null() {
        return -EINVAL;
    }

    // SAFETY: `state` and `crtc` are non-null and valid per caller preconditions.
    let crtc_state = unsafe { drm_atomic_get_crtc_state(state, crtc) };
    if IS_ERR(crtc_state) {
        return PTR_ERR(crtc_state);
    }

    // SAFETY: `state` points to an initialized `DrmAtomicCommit`.
    let dev = unsafe { (*state).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
    let ret = unsafe {
        // SAFETY: `config.connection_mutex` is an initialized modeset lock and `state.acquire_ctx` is valid.
        drm_modeset_lock(
            core::ptr::addr_of_mut!((*config).connection_mutex),
            (*state).acquire_ctx,
        )
    };
    if ret != 0 {
        return ret;
    }

    let mut conn_iter = core::mem::MaybeUninit::<DrmConnectorListIter>::uninit();
    unsafe {
        // SAFETY: `dev` is valid, and `conn_iter` memory is uninitialized storage for the iterator.
        drm_connector_list_iter_begin(dev, conn_iter.as_mut_ptr());
        let iter = conn_iter.as_mut_ptr();
        loop {
            let connector = drm_connector_list_iter_next(iter);
            if connector.is_null() {
                break;
            }
            if ((*crtc_state).connector_mask & drm_connector_mask(connector)) == 0 {
                continue;
            }

            let conn_state = drm_atomic_get_connector_state(state, connector);
            if IS_ERR(conn_state) {
                drm_connector_list_iter_end(iter);
                return PTR_ERR(conn_state);
            }
        }
        drm_connector_list_iter_end(iter);
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_add_affected_planes()
/// Adds all planes previously associated with `crtc` to the atomic commit.
///
/// # Safety
///
/// - `state` must point to an initialized `DrmAtomicCommit`.
/// - `crtc` must point to a live `DrmCrtc` belonging to the same device as `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_add_affected_planes(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> i32 {
    if state.is_null() || crtc.is_null() {
        return -EINVAL;
    }

    // SAFETY: `state` and `crtc` are non-null and valid per caller preconditions.
    let old_crtc_state = unsafe { drm_atomic_get_old_crtc_state(state, crtc) };
    if old_crtc_state.is_null() {
        return 0;
    }

    // SAFETY: `state` points to an initialized `DrmAtomicCommit`.
    let dev = unsafe { (*state).dev };
    // SAFETY: `old_crtc_state` was checked non-null above.
    let plane_mask = unsafe { (*old_crtc_state).plane_mask };
    // SAFETY: `dev` points to an initialized `DrmDevice`.
    let head = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.plane_list) };
    // SAFETY: `head` is a valid list head in `dev.mode_config`.
    let mut link = unsafe { (*head).next };

    while link != head {
        let plane = unsafe {
            // SAFETY: `link` is an embedded `head` member of a live `DrmPlane` in `plane_list`.
            (link as *mut u8).sub(core::mem::offset_of!(DrmPlane, head)) as *mut DrmPlane
        };
        // SAFETY: `link` points to a valid list node.
        link = unsafe { (*link).next };

        // SAFETY: `plane` is a valid `DrmPlane` pointer.
        if (plane_mask & (1u32 << unsafe { (*plane).index })) == 0 {
            continue;
        }

        // SAFETY: `state` and `plane` are valid pointers.
        let plane_state = unsafe { drm_atomic_get_plane_state(state, plane) };
        if IS_ERR(plane_state) {
            return PTR_ERR(plane_state);
        }
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_add_affected_colorops()
/// Stub to add affected colorop states.
///
/// # Safety
///
/// No caller preconditions; always returns 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_add_affected_colorops(
    _state: *mut DrmAtomicCommit,
    _plane: *mut DrmPlane,
) -> i32 {
    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_check_only()
/// Validates an atomic commit without applying it (check-only mode).
///
/// # Safety
///
/// `state` must point to an initialized `DrmAtomicCommit` with valid `dev` and populated state arrays.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_check_only(state: *mut DrmAtomicCommit) -> i32 {
    if state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `state` was verified non-null above.
    let dev = unsafe { (*state).dev };
    if dev.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` was verified non-null above.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    unsafe {
        // SAFETY: `config` is valid; iterating over plane entries within allocated bounds.
        let num_planes = (*config).num_total_plane as usize;
        for i in 0..num_planes {
            if (*state).planes.is_null() {
                break;
            }
            let entry = (*state).planes.add(i);
            if (*entry).ptr.is_null() {
                continue;
            }
            let ret = drm_atomic_plane_check((*entry).old_state, (*entry).new_state);
            if ret != 0 {
                return ret;
            }
        }

        let num_crtcs = (*config).num_crtc as usize;
        for i in 0..num_crtcs {
            if (*state).crtcs.is_null() {
                break;
            }
            let entry = (*state).crtcs.add(i);
            if (*entry).ptr.is_null() {
                continue;
            }
            let ret = drm_atomic_crtc_check((*entry).old_state, (*entry).new_state);
            if ret != 0 {
                return ret;
            }
        }

        let num_conns = (*state).num_connector as usize;
        for i in 0..num_conns {
            if (*state).connectors.is_null() {
                break;
            }
            let entry = (*state).connectors.add(i);
            if (*entry).ptr.is_null() {
                continue;
            }
            let ret = drm_atomic_connector_check((*entry).ptr, (*entry).new_state);
            if ret != 0 {
                return ret;
            }
        }

        let funcs = (*config).funcs;
        if !funcs.is_null()
            && let Some(check_fn) = (*funcs).atomic_check
        {
            let ret = check_fn(dev, state);
            if ret != 0 {
                return ret;
            }
        }

        if !(*state).allow_modeset() {
            for i in 0..num_crtcs {
                if (*state).crtcs.is_null() {
                    break;
                }
                let entry = (*state).crtcs.add(i);
                if (*entry).ptr.is_null() {
                    continue;
                }
                let new_state = (*entry).new_state;
                if drm_atomic_crtc_needs_modeset(new_state) {
                    return -EINVAL;
                }
            }
        }

        (*state).set_checked(true);
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_commit()
/// Commits the atomic state change to hardware (blocking).
///
/// # Safety
///
/// `state` must point to an initialized `DrmAtomicCommit` whose objects and locks
/// are held according to atomic modesetting rules.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_commit(state: *mut DrmAtomicCommit) -> i32 {
    if state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `state` was checked non-null above.
    let dev = unsafe { (*state).dev };
    if dev.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` was verified non-null above.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    // SAFETY: `state` is a valid `DrmAtomicCommit` pointer.
    let ret = unsafe { drm_atomic_check_only(state) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `config` is a valid pointer to `dev.mode_config`.
    let funcs = unsafe { (*config).funcs };
    // SAFETY: `funcs` was checked non-null.
    if funcs.is_null() || unsafe { (*funcs).atomic_commit.is_none() } {
        return -EOPNOTSUPP;
    }

    // SAFETY: `atomic_commit` hook is non-null and valid.
    unsafe { ((*funcs).atomic_commit.unwrap())(dev, state, false) }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_nonblocking_commit()
/// Commits the atomic state change to hardware asynchronously (non-blocking).
///
/// # Safety
///
/// `state` must point to an initialized `DrmAtomicCommit` whose objects and locks
/// are held according to atomic modesetting rules.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_nonblocking_commit(state: *mut DrmAtomicCommit) -> i32 {
    if state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `state` was checked non-null above.
    let dev = unsafe { (*state).dev };
    if dev.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` was verified non-null above.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    // SAFETY: `state` is a valid `DrmAtomicCommit` pointer.
    let ret = unsafe { drm_atomic_check_only(state) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `config` is a valid pointer to `dev.mode_config`.
    let funcs = unsafe { (*config).funcs };
    // SAFETY: `funcs` was checked non-null.
    if funcs.is_null() || unsafe { (*funcs).atomic_commit.is_none() } {
        return -EOPNOTSUPP;
    }

    // SAFETY: `atomic_commit` hook is non-null and valid.
    unsafe { ((*funcs).atomic_commit.unwrap())(dev, state, true) }
}

// upstream: drivers/gpu/drm/drm_atomic.c __drm_atomic_helper_disable_plane()
/// Helper to clear plane coordinates, CRTC, and framebuffer in a plane state to disable it.
///
/// # Safety
///
/// `plane_state` must point to a valid, mutable `DrmPlaneState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_disable_plane(
    _plane: *mut DrmPlane,
    plane_state: *mut DrmPlaneState,
) -> i32 {
    if plane_state.is_null() {
        return -EINVAL;
    }

    let ret = unsafe {
        // SAFETY: `plane_state` was verified non-null above and points to a mutable `DrmPlaneState`.
        crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_plane(
            plane_state,
            core::ptr::null_mut(),
        )
    };
    if ret != 0 {
        return ret;
    }

    unsafe {
        // SAFETY: `plane_state` is non-null and mutable.
        crate::drm_atomic_uapi_upstream::drm_atomic_set_fb_for_plane(
            plane_state,
            core::ptr::null_mut(),
        );
        (*plane_state).crtc_x = 0;
        (*plane_state).crtc_y = 0;
        (*plane_state).crtc_w = 0;
        (*plane_state).crtc_h = 0;
        (*plane_state).src_x = 0;
        (*plane_state).src_y = 0;
        (*plane_state).src_w = 0;
        (*plane_state).src_h = 0;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c update_output_state()
/// Internal helper to synchronize connector and CRTC states with a legacy modeset configuration.
///
/// # Safety
///
/// - `state` must point to an initialized `DrmAtomicCommit`.
/// - `set` must point to a valid `DrmModeSet` with an initialized `crtc`.
unsafe fn update_output_state(state: *mut DrmAtomicCommit, set: *mut DrmModeSet) -> i32 {
    // SAFETY: `set` and `set.crtc` are valid pointers per caller preconditions.
    let dev = unsafe { (*(*set).crtc).dev };
    let ret = unsafe {
        // SAFETY: `dev.mode_config.connection_mutex` is an initialized modeset lock and `state.acquire_ctx` is valid.
        drm_modeset_lock(
            core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex),
            (*state).acquire_ctx,
        )
    };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `state` and `set.crtc` are valid pointers.
    let ret = unsafe { drm_atomic_add_affected_connectors(state, (*set).crtc) };
    if ret != 0 {
        return ret;
    }

    unsafe {
        // SAFETY: `state` points to an initialized `DrmAtomicCommit`.
        let num_conns = (*state).num_connector as usize;
        for i in 0..num_conns {
            let entry = (*state).connectors.add(i);
            if (*entry).ptr.is_null() {
                continue;
            }
            let new_conn_state = (*entry).new_state;
            if (*new_conn_state).crtc == (*set).crtc {
                let ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_connector(
                    new_conn_state,
                    core::ptr::null_mut(),
                );
                if ret != 0 {
                    return ret;
                }
                (*new_conn_state).link_status = DRM_LINK_STATUS_GOOD;
            }
        }

        let num_set_conns = (*set).num_connectors;
        for i in 0..num_set_conns {
            let conn = *(*set).connectors.add(i);
            let new_conn_state = drm_atomic_get_connector_state(state, conn);
            if IS_ERR(new_conn_state) {
                return PTR_ERR(new_conn_state);
            }
            let ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_connector(
                new_conn_state,
                (*set).crtc,
            );
            if ret != 0 {
                return ret;
            }
        }

        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            if crtc.is_null() || crtc == (*set).crtc {
                continue;
            }
            let new_crtc_state = (*entry).new_state;
            if (*new_crtc_state).connector_mask == 0 {
                let ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_mode_prop_for_crtc(
                    new_crtc_state,
                    core::ptr::null_mut(),
                );
                if ret < 0 {
                    return ret;
                }
                (*new_crtc_state).active = false;
            }
        }
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic.c __drm_atomic_helper_set_config()
/// Translates a legacy `drm_mode_set_config` request into atomic state updates.
///
/// # Safety
///
/// - `set` must point to an initialized `DrmModeSet` with a valid `crtc`.
/// - `state` must point to an initialized `DrmAtomicCommit`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_set_config(
    set: *mut DrmModeSet,
    state: *mut DrmAtomicCommit,
) -> i32 {
    if set.is_null() || state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `set` was verified non-null above.
    let crtc = unsafe { (*set).crtc };
    if crtc.is_null() {
        return -EINVAL;
    }

    // SAFETY: `state` and `crtc` are valid pointers.
    let crtc_state = unsafe { drm_atomic_get_crtc_state(state, crtc) };
    if IS_ERR(crtc_state) {
        return PTR_ERR(crtc_state);
    }

    // SAFETY: `crtc` is valid and points to an initialized `DrmCrtc`.
    let primary = unsafe { (*crtc).primary };
    // SAFETY: `state` and `primary` are valid pointers.
    let primary_state = unsafe { drm_atomic_get_plane_state(state, primary) };
    if IS_ERR(primary_state) {
        return PTR_ERR(primary_state);
    }

    // SAFETY: `set` is a valid `DrmModeSet` pointer.
    if unsafe { (*set).mode.is_null() } {
        let ret = unsafe {
            // SAFETY: `crtc_state` is a valid pointer returned from `drm_atomic_get_crtc_state`.
            crate::drm_atomic_uapi_upstream::drm_atomic_set_mode_for_crtc(
                crtc_state,
                core::ptr::null(),
            )
        };
        if ret != 0 {
            return ret;
        }
        unsafe {
            // SAFETY: `crtc_state` is a valid mutable pointer.
            (*crtc_state).active = false;
        }

        let ret = unsafe {
            // SAFETY: `primary_state` is a valid mutable pointer.
            crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_plane(
                primary_state,
                core::ptr::null_mut(),
            )
        };
        if ret != 0 {
            return ret;
        }

        unsafe {
            // SAFETY: `primary_state` is a valid mutable pointer.
            crate::drm_atomic_uapi_upstream::drm_atomic_set_fb_for_plane(
                primary_state,
                core::ptr::null_mut(),
            );
        }

        // SAFETY: `state` and `set` are valid pointers.
        return unsafe { update_output_state(state, set) };
    }

    let ret = unsafe {
        // SAFETY: `crtc_state` is valid and `(*set).mode` is non-null.
        crate::drm_atomic_uapi_upstream::drm_atomic_set_mode_for_crtc(crtc_state, (*set).mode)
    };
    if ret != 0 {
        return ret;
    }
    unsafe {
        // SAFETY: `crtc_state` is a valid mutable pointer.
        (*crtc_state).active = true;
    }

    let ret = unsafe {
        // SAFETY: `primary_state` and `crtc` are valid pointers.
        crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_plane(primary_state, crtc)
    };
    if ret != 0 {
        return ret;
    }

    let mut hdisplay: i32 = 0;
    let mut vdisplay: i32 = 0;
    unsafe {
        // SAFETY: `(*set).mode` is non-null and valid; timings are written to local variables.
        crate::drm_modes_upstream::drm_mode_get_hv_timing(
            (*set).mode,
            &mut hdisplay,
            &mut vdisplay,
        );
        crate::drm_atomic_uapi_upstream::drm_atomic_set_fb_for_plane(primary_state, (*set).fb);
        (*primary_state).crtc_x = 0;
        (*primary_state).crtc_y = 0;
        (*primary_state).crtc_w = hdisplay as u32;
        (*primary_state).crtc_h = vdisplay as u32;
        (*primary_state).src_x = (*set).x << 16;
        (*primary_state).src_y = (*set).y << 16;

        if drm_rotation_90_or_270((*primary_state).rotation) {
            (*primary_state).src_w = (vdisplay as u32) << 16;
            (*primary_state).src_h = (hdisplay as u32) << 16;
        } else {
            (*primary_state).src_w = (hdisplay as u32) << 16;
            (*primary_state).src_h = (vdisplay as u32) << 16;
        }
    }

    // SAFETY: `state` and `set` are valid pointers.
    unsafe { update_output_state(state, set) }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_private_obj_print_state()
/// Prints human-readable details of a private object state.
///
/// # Safety
///
/// If non-null, `p` must point to a valid `DrmPrinter`.
/// If non-null, `state` must point to a valid `DrmPrivateState`.
unsafe fn drm_atomic_private_obj_print_state(p: *mut DrmPrinter, state: *const DrmPrivateState) {
    if p.is_null() || state.is_null() {
        return;
    }
    // SAFETY: `state` was verified non-null above and points to a valid `DrmPrivateState`.
    let obj = unsafe { (*state).obj };
    if obj.is_null() {
        return;
    }
    // SAFETY: `obj` was checked non-null above.
    let funcs = unsafe { (*obj).funcs };
    if !funcs.is_null()
        // SAFETY: `funcs` is non-null.
        && let Some(print_fn) = unsafe { (*funcs).atomic_print_state }
    {
        unsafe {
            // SAFETY: `print_fn` is a valid driver callback, `p` is non-null, and `state` is a valid `DrmPrivateState`.
            print_fn(p, state as *mut DrmPrivateState);
        }
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_atomic_print_new_state()
/// Prints the entire new atomic state (all planes, CRTCs, connectors, and private objects).
///
/// # Safety
///
/// If non-null, `state` must point to an initialized `DrmAtomicCommit`.
/// If non-null, `p` must point to a valid `DrmPrinter`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_print_new_state(
    state: *const DrmAtomicCommit,
    p: *mut DrmPrinter,
) {
    if p.is_null() || state.is_null() {
        return;
    }
    // SAFETY: `state` was verified non-null above.
    let dev = unsafe { (*state).dev };
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` was verified non-null above.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    unsafe {
        // SAFETY: `config` is valid; iterate within allocated bounds of `planes`, `crtcs`, `connectors`, `private_objs`.
        let num_planes = (*config).num_total_plane as usize;
        for i in 0..num_planes {
            if (*state).planes.is_null() {
                break;
            }
            let entry = (*state).planes.add(i);
            if !(*entry).new_state.is_null() {
                drm_atomic_plane_print_state(p, (*entry).new_state);
            }
        }

        let num_crtcs = (*config).num_crtc as usize;
        for i in 0..num_crtcs {
            if (*state).crtcs.is_null() {
                break;
            }
            let entry = (*state).crtcs.add(i);
            if !(*entry).new_state.is_null() {
                drm_atomic_crtc_print_state(p, (*entry).new_state);
            }
        }

        let num_conns = (*state).num_connector as usize;
        for i in 0..num_conns {
            if (*state).connectors.is_null() {
                break;
            }
            let entry = (*state).connectors.add(i);
            if !(*entry).new_state.is_null() {
                drm_atomic_connector_print_state(p, (*entry).new_state);
            }
        }

        let num_priv = (*state).num_private_objs as usize;
        for i in 0..num_priv {
            if (*state).private_objs.is_null() {
                break;
            }
            let entry = (*state).private_objs.add(i);
            if !(*entry).new_state.is_null() {
                drm_atomic_private_obj_print_state(p, (*entry).new_state);
            }
        }
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c __drm_state_dump()
/// Dumps all active DRM device states to `p`.
///
/// # Safety
///
/// `dev` must point to an initialized `DrmDevice`.
/// If `take_locks` is false, caller must ensure no concurrent mutations occur to device states.
/// `p` must point to a valid `DrmPrinter`.
unsafe fn __drm_state_dump(dev: *mut DrmDevice, p: *mut DrmPrinter, take_locks: bool) {
    if dev.is_null() || p.is_null() {
        return;
    }
    // SAFETY: `dev` was verified non-null above.
    if !unsafe { drm_drv_uses_atomic_modeset(dev) } {
        return;
    }
    // SAFETY: `dev` was verified non-null above.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    unsafe {
        // SAFETY: `config` is valid; traverse registered plane, crtc, connector, and private object lists.
        let plane_head = core::ptr::addr_of_mut!((*config).plane_list);
        let mut link = (*plane_head).next;
        while link != plane_head {
            let plane =
                (link as *mut u8).sub(core::mem::offset_of!(DrmPlane, head)) as *mut DrmPlane;
            link = (*link).next;

            if take_locks {
                drm_modeset_lock(
                    core::ptr::addr_of_mut!((*plane).mutex),
                    core::ptr::null_mut(),
                );
            }
            drm_atomic_plane_print_state(p, (*plane).state);
            if take_locks {
                drm_modeset_unlock(core::ptr::addr_of_mut!((*plane).mutex));
            }
        }

        let crtc_head = core::ptr::addr_of_mut!((*config).crtc_list);
        let mut link = (*crtc_head).next;
        while link != crtc_head {
            let crtc = (link as *mut u8).sub(core::mem::offset_of!(DrmCrtc, head)) as *mut DrmCrtc;
            link = (*link).next;

            if take_locks {
                drm_modeset_lock(
                    core::ptr::addr_of_mut!((*crtc).mutex),
                    core::ptr::null_mut(),
                );
            }
            drm_atomic_crtc_print_state(p, (*crtc).state);
            if take_locks {
                drm_modeset_unlock(core::ptr::addr_of_mut!((*crtc).mutex));
            }
        }

        let mut conn_iter = core::mem::MaybeUninit::<DrmConnectorListIter>::uninit();
        drm_connector_list_iter_begin(dev, conn_iter.as_mut_ptr());
        let iter = conn_iter.as_mut_ptr();
        if take_locks {
            drm_modeset_lock(
                core::ptr::addr_of_mut!((*config).connection_mutex),
                core::ptr::null_mut(),
            );
        }
        loop {
            let connector = drm_connector_list_iter_next(iter);
            if connector.is_null() {
                break;
            }
            drm_atomic_connector_print_state(p, (*connector).state);
        }
        if take_locks {
            drm_modeset_unlock(core::ptr::addr_of_mut!((*config).connection_mutex));
        }
        drm_connector_list_iter_end(iter);

        let priv_head = core::ptr::addr_of_mut!((*config).privobj_list);
        let mut link = (*priv_head).next;
        while link != priv_head {
            let obj = (link as *mut u8).sub(core::mem::offset_of!(DrmPrivateObj, head))
                as *mut DrmPrivateObj;
            link = (*link).next;

            if take_locks {
                drm_modeset_lock(core::ptr::addr_of_mut!((*obj).lock), core::ptr::null_mut());
            }
            drm_atomic_private_obj_print_state(p, (*obj).state);
            if take_locks {
                drm_modeset_unlock(core::ptr::addr_of_mut!((*obj).lock));
            }
        }
    }
}

// upstream: drivers/gpu/drm/drm_atomic.c drm_state_dump()
/// Dumps current DRM device state to printer without acquiring modeset locks.
///
/// # Safety
///
/// `dev` must point to an initialized `DrmDevice`.
/// `p` must point to a valid `DrmPrinter`.
/// Caller must ensure no concurrent state changes occur while dumping.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_state_dump(dev: *mut DrmDevice, p: *mut DrmPrinter) {
    unsafe {
        // SAFETY: `dev` and `p` are passed to `__drm_state_dump` with `take_locks = false`;
        // caller guarantees no concurrent mutations per preconditions.
        __drm_state_dump(dev, p, false)
    };
}
