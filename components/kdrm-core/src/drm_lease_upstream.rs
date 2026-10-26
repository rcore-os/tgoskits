// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! DRM display resource leasing implementation.
//!
//! Provides support for leasing subsets of display resources (CRTCs, planes,
//! and connectors) from a primary DRM master to secondary lessee processes,
//! enabling independent control over separate display outputs.

use core::{
    ffi::{c_int, c_void},
    sync::atomic::Ordering,
};

#[cfg(not(test))]
use drm_kapi::mutex::{mutex_lock, mutex_unlock};
use drm_kapi::{
    config::{
        EBUSY, EFAULT, EINVAL, ENODEV, ENOMEM, EOPNOTSUPP, GFP_KERNEL, err_ptr, is_err, ptr_err,
    },
    idr::{Idr, idr_get_next},
    kernel_services::ProviderSlot,
    list::{list_add_tail, list_del},
    memory::{kfree, kmalloc},
    mm_native::{copy_from_user, copy_to_user},
    types::{File, ListHead},
};
#[cfg(test)]
unsafe fn mutex_lock<M>(_mutex: M) {}
#[cfg(test)]
unsafe fn mutex_unlock<M>(_mutex: M) {}

use crate::{
    drm_auth_types::DrmMaster,
    drm_auth_upstream::{drm_file_get_master, drm_master_create, drm_master_get, drm_master_put},
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::{
        DRIVER_MODESET, drm_core_check_feature, drm_mode_object_find, drm_mode_object_put,
        idr_alloc, idr_destroy, idr_find, idr_init, idr_remove,
    },
    uapi_drm_consts::{DRM_MODE_OBJECT_CONNECTOR, DRM_MODE_OBJECT_CRTC, DRM_MODE_OBJECT_PLANE},
    uapi_drm_mode_types::{
        DrmModeCreateLease, DrmModeGetLease, DrmModeListLessees, DrmModeRevokeLease,
    },
};

const EACCES: i32 = 13;
const ENOENT: i32 = 2;
const O_NONBLOCK: u32 = 0o4000;
const O_CLOEXEC: u32 = 0o2000000;

/// Initializes a freshly opened lessee `drm_file` before its descriptor is
/// published. `ctx` is the context passed to [`LeaseFileOps::open_lessee_fd`].
pub type LesseeFileSetup = unsafe extern "C" fn(file_priv: *mut DrmFile, ctx: *mut c_void);

/// Operating-system service that gives a lessee its own DRM file description.
#[repr(C)]
pub struct LeaseFileOps {
    /// Opens a new file description on the DRM minor behind `lessor_filp` and
    /// returns its descriptor, or a negative errno.
    ///
    /// The provider must finish every fallible step (descriptor reservation
    /// and DRM open) first, then call `setup` exactly once with the new
    /// `drm_file`, and then install the descriptor with `O_CLOEXEC` taken from
    /// `flags`. On failure it must not call `setup`.
    pub open_lessee_fd: unsafe extern "C" fn(
        lessor_filp: *mut File,
        flags: c_int,
        setup: LesseeFileSetup,
        ctx: *mut c_void,
    ) -> c_int,
}

static LEASE_FILES: ProviderSlot<LeaseFileOps> = ProviderSlot::new();

/// Installs the provider used by `DRM_IOCTL_MODE_CREATE_LEASE` to create
/// lessee file descriptors.
pub fn install_lease_file_ops(ops: &'static LeaseFileOps) -> Result<(), &'static str> {
    LEASE_FILES.install(ops)
}

/// Makes a newly opened file the lessee's master file.
///
/// # Safety
/// `file_priv` must be a live, not yet published `drm_file`, and `ctx` must be
/// a referenced lessee `DrmMaster` whose reference is transferred to the file.
unsafe extern "C" fn adopt_lessee_master(file_priv: *mut DrmFile, ctx: *mut c_void) {
    // SAFETY: the provider calls this before the descriptor is installed, so
    // no other thread can reach `file_priv`. The open path gave the file its
    // own master reference, which is dropped before the lessee reference
    // supplied by the caller takes its place.
    unsafe {
        drm_master_put(core::ptr::addr_of_mut!((*file_priv).master));
        (*file_priv).master = ctx.cast::<DrmMaster>();
        (*file_priv)
            .is_master
            .store(true, core::sync::atomic::Ordering::Relaxed);
        (*file_priv)
            .authenticated
            .store(true, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Non-null sentinel pointer stored in lease IDRs to indicate object membership.
static LEASE_OBJECT_TOKEN: u8 = 1;

/// Derives the enclosing `DrmMaster` pointer from its embedded `lessee_list` member.
#[inline]
fn master_from_lessee_list(node: *mut ListHead) -> *mut DrmMaster {
    node.cast::<u8>()
        .wrapping_sub(core::mem::offset_of!(DrmMaster, lessee_list))
        .cast::<DrmMaster>()
}

/// Derives the enclosing `DrmCrtc` pointer from its embedded `head` member.
#[inline]
fn crtc_from_head(node: *mut ListHead) -> *mut DrmCrtc {
    node.cast::<u8>()
        .wrapping_sub(core::mem::offset_of!(DrmCrtc, head))
        .cast::<DrmCrtc>()
}

/// Traverses direct child lessees attached to `lessor`.
///
/// # Safety
/// `lessor` must point to a live `DrmMaster`. Caller must hold `mode_config.idr_mutex`
/// to protect list iteration against concurrent modification.
unsafe fn for_each_lessee<F>(lessor: *mut DrmMaster, mut f: F)
where
    F: FnMut(*mut DrmMaster) -> bool,
{
    // SAFETY: `lessor` is live and callers synchronize list traversal with `idr_mutex`.
    unsafe {
        let head = core::ptr::addr_of_mut!((*lessor).lessees);
        let mut curr = (*head).next;
        while curr != head && !curr.is_null() {
            let lessee = master_from_lessee_list(curr);
            let next_node = (*curr).next;
            if !f(lessee) {
                break;
            }
            curr = next_node;
        }
    }
}

/// Locates the root owner of the lease hierarchy by traversing up `lessor` links.
#[unsafe(no_mangle)]
/// # Safety
/// If `master` is non-null, it must point to an initialized `DrmMaster`.
pub unsafe extern "C" fn drm_lease_owner(mut master: *mut DrmMaster) -> *mut DrmMaster {
    if master.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `master` is non-null. We traverse upward along `lessor` links
    // until reaching the root master whose `lessor` pointer is null.
    unsafe {
        while !(*master).lessor.is_null() {
            master = (*master).lessor;
        }
        master
    }
}

/// Looks up a lessee master by its ID within the lease tree of `master`.
///
/// # Safety
/// `master` must be a valid pointer and caller must hold `mode_config.idr_mutex`.
unsafe fn find_lessee_master(master: *mut DrmMaster, lessee_id: i32) -> *mut DrmMaster {
    // SAFETY: `master` is valid and caller holds `idr_mutex`.
    unsafe {
        let owner = drm_lease_owner(master);
        if owner.is_null() {
            return core::ptr::null_mut();
        }
        idr_find(
            core::ptr::addr_of_mut!((*owner).lessee_idr),
            lessee_id as core::ffi::c_ulong,
        )
        .cast()
    }
}

/// Checks whether `master` holds access rights to mode object `id`.
///
/// # Safety
/// `master` must point to a live `DrmMaster`.
unsafe fn master_holds_object(master: *mut DrmMaster, id: i32) -> bool {
    // SAFETY: `master` is live. If it is a root master (lessor is null), it owns all objects.
    // Otherwise, membership is determined by searching its `leases` IDR.
    unsafe {
        if (*master).lessor.is_null() {
            return true;
        }
        let found = idr_find(
            core::ptr::addr_of_mut!((*master).leases),
            id as core::ffi::c_ulong,
        );
        !found.is_null()
    }
}

/// Determines whether mode object `id` has already been leased out to an active child lessee.
///
/// # Safety
/// `master` must be valid and caller must hold `mode_config.idr_mutex`.
unsafe fn is_object_already_leased(master: *mut DrmMaster, id: i32) -> bool {
    let mut leased = false;
    // SAFETY: `master` is valid and traversal is protected by `idr_mutex`.
    unsafe {
        for_each_lessee(master, |lessee| {
            if master_holds_object(lessee, id) {
                leased = true;
                false
            } else {
                true
            }
        });
    }
    leased
}

/// Checks whether the calling file holds lease access to mode object `id` without acquiring locks.
#[unsafe(no_mangle)]
/// # Safety
/// If `file_priv` is non-null, it must point to an initialized `DrmFile`.
/// Caller must ensure appropriate synchronization for IDR lookups (e.g. `idr_mutex` already held).
pub unsafe extern "C" fn _drm_lease_held(file_priv: *mut DrmFile, id: i32) -> bool {
    if file_priv.is_null() {
        return true;
    }
    // SAFETY: `file_priv` is non-null. We retrieve the associated master, check object access,
    // and release the acquired master reference.
    unsafe {
        let mut master = drm_file_get_master(file_priv);
        if master.is_null() {
            return true;
        }
        let held = master_holds_object(master, id);
        drm_master_put(core::ptr::addr_of_mut!(master));
        held
    }
}

/// Checks whether the calling file holds lease access to mode object `id`, acquiring locks as needed.
#[unsafe(no_mangle)]
/// # Safety
/// If `file_priv` is non-null, it must point to an initialized `DrmFile`.
pub unsafe extern "C" fn drm_lease_held(file_priv: *mut DrmFile, id: i32) -> bool {
    if file_priv.is_null() {
        return true;
    }
    // SAFETY: `file_priv` is non-null. If the master is a lessee, we protect the IDR search
    // with the device `idr_mutex`.
    unsafe {
        let mut master = drm_file_get_master(file_priv);
        if master.is_null() {
            return true;
        }
        if (*master).lessor.is_null() {
            drm_master_put(core::ptr::addr_of_mut!(master));
            return true;
        }

        let mutex_ptr = core::ptr::addr_of_mut!((*(*master).dev).mode_config.idr_mutex);
        mutex_lock(mutex_ptr);
        let held = master_holds_object(master, id);
        mutex_unlock(mutex_ptr);

        drm_master_put(core::ptr::addr_of_mut!(master));
        held
    }
}

/// Filters and remaps a bitmask of possible CRTCs to match the caller's leased view.
#[unsafe(no_mangle)]
/// # Safety
/// If `file_priv` is non-null, it must point to an initialized `DrmFile`.
pub unsafe extern "C" fn drm_lease_filter_crtcs(file_priv: *mut DrmFile, crtcs_in: u32) -> u32 {
    if file_priv.is_null() {
        return crtcs_in;
    }

    // SAFETY: `file_priv` is non-null.
    unsafe {
        let mut master = drm_file_get_master(file_priv);
        if master.is_null() {
            return crtcs_in;
        }

        // An unleased master has full access to all CRTCs
        if (*master).lessor.is_null() {
            drm_master_put(core::ptr::addr_of_mut!(master));
            return crtcs_in;
        }

        let dev = (*master).dev;
        let mutex_ptr = core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex);
        mutex_lock(mutex_ptr);

        let mut crtcs_out = 0u32;
        let mut in_idx = 0u32;
        let mut out_idx = 0u32;

        let crtc_head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut curr = (*crtc_head).next;
        while curr != crtc_head && !curr.is_null() {
            let crtc = crtc_from_head(curr);
            let crtc_id = (*crtc).base.id as i32;

            if master_holds_object(master, crtc_id) {
                if in_idx < 32 && out_idx < 32 && (crtcs_in & (1u32 << in_idx)) != 0 {
                    crtcs_out |= 1u32 << out_idx;
                }
                out_idx += 1;
            }
            in_idx += 1;
            curr = (*curr).next;
        }

        mutex_unlock(mutex_ptr);
        drm_master_put(core::ptr::addr_of_mut!(master));
        crtcs_out
    }
}

/// Checks and publishes the final lease set atomically, returning an error
/// pointer if any object disappeared, is already leased, or allocation fails.
///
/// # Safety
/// `lessor` must be a referenced root master on a live device. `leases` must
/// be an exclusively owned, initialized IDR of mode object IDs. The caller
/// must not hold `idr_mutex`. Success transfers the set; failure leaves it owned
/// by the caller.
unsafe fn create_lessee_master(lessor: *mut DrmMaster, leases: *mut Idr) -> *mut DrmMaster {
    // SAFETY: The caller owns the unpublished IDR and retains `lessor` and its
    // device. `idr_mutex` protects all published object/lease lookups through
    // publication, excluding concurrent creation, destruction and revocation.
    unsafe {
        let dev = (*lessor).dev;
        let lessee = drm_master_create(dev);
        if lessee.is_null() {
            return err_ptr(-ENOMEM);
        }

        let mutex_ptr = core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex);
        mutex_lock(mutex_ptr);

        let result = (|| {
            let mut object = 0;
            while !idr_get_next(leases.cast(), &mut object).is_null() {
                if idr_find(
                    core::ptr::addr_of_mut!((*dev).mode_config.object_idr),
                    object as core::ffi::c_ulong,
                )
                .is_null()
                {
                    return Err(-ENOENT);
                }
                if is_object_already_leased(lessor, object) {
                    return Err(-EBUSY);
                }
                object += 1;
            }

            let owner = drm_lease_owner(lessor);
            let id_alloc = idr_alloc(
                core::ptr::addr_of_mut!((*owner).lessee_idr),
                lessee.cast(),
                1,
                0,
            );
            if id_alloc < 0 {
                return Err(id_alloc);
            }

            (*lessee).lessee_id = id_alloc;
            (*lessee).lessor = drm_master_get(lessor);
            core::ptr::swap(core::ptr::addr_of_mut!((*lessee).leases), leases);
            list_add_tail(
                core::ptr::addr_of_mut!((*lessee).lessee_list),
                core::ptr::addr_of_mut!((*lessor).lessees),
            );
            Ok(())
        })();

        mutex_unlock(mutex_ptr);
        if let Err(error) = result {
            // Destruction can acquire `idr_mutex`, so drop the unpublished
            // master only after unlocking. The caller still owns `leases`.
            let mut lessee = lessee;
            drm_master_put(core::ptr::addr_of_mut!(lessee));
            err_ptr(error)
        } else {
            lessee
        }
    }
}

/// Unregisters and cleans up leasing structures associated with `master`.
#[unsafe(no_mangle)]
/// # Safety
/// If `master` is non-null, it must point to an allocated `DrmMaster` being destroyed.
pub unsafe extern "C" fn drm_lease_destroy(master: *mut DrmMaster) {
    if master.is_null() {
        return;
    }
    // SAFETY: `master` is non-null. We unregister the lessee ID and unlink from lessor's list.
    unsafe {
        let mut lessor = (*master).lessor;
        if lessor.is_null() {
            return;
        }

        let dev = (*master).dev;
        let mutex_ptr = core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex);
        mutex_lock(mutex_ptr);

        let owner = drm_lease_owner(lessor);
        if !owner.is_null() {
            idr_remove(
                core::ptr::addr_of_mut!((*owner).lessee_idr),
                (*master).lessee_id,
            );
        }
        list_del(core::ptr::addr_of_mut!((*master).lessee_list));

        mutex_unlock(mutex_ptr);

        drm_master_put(core::ptr::addr_of_mut!(lessor));
        (*master).lessor = core::ptr::null_mut();
    }
}

/// Recursively empties the lease maps of `top` and all of its descendant lessees.
///
/// # Safety
/// `top` must point to a live `DrmMaster`. Caller must hold `mode_config.idr_mutex`.
unsafe fn revoke_lease_hierarchy(top: *mut DrmMaster) {
    // SAFETY: Recursive traversal of active lessees protected by caller's lock on `idr_mutex`.
    unsafe {
        for_each_lessee(top, |sub_lessee| {
            revoke_lease_hierarchy(sub_lessee);
            true
        });

        let lease_idr = core::ptr::addr_of_mut!((*top).leases);
        idr_destroy(lease_idr);
        idr_init(lease_idr);
    }
}

/// Revokes all resources leased to `top` and all its subordinate lessees.
#[unsafe(no_mangle)]
/// # Safety
/// If `top` is non-null, it must point to an initialized `DrmMaster`.
pub unsafe extern "C" fn drm_lease_revoke(top: *mut DrmMaster) {
    if top.is_null() {
        return;
    }
    // SAFETY: `top` is non-null. We acquire `idr_mutex` to wipe the lease hierarchy.
    unsafe {
        let dev = (*top).dev;
        let mutex_ptr = core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex);
        mutex_lock(mutex_ptr);
        revoke_lease_hierarchy(top);
        mutex_unlock(mutex_ptr);
    }
}

/// Validates that requested lease objects only comprise allowed display object types,
/// and requires an explicit plane when the client uses universal planes.
///
/// # Safety
/// `objects` must point to an array of `count` valid `DrmModeObject` pointers.
unsafe fn validate_lease_objects(
    count: usize,
    objects: *const *mut DrmModeObject,
    universal_planes: bool,
) -> i32 {
    let mut has_crtc = false;
    let mut has_connector = false;
    let mut has_plane = false;

    for i in 0..count {
        // SAFETY: `i` is within `0..count`, and `objects` points to `count` entries.
        let obj_ptr = unsafe { *objects.add(i) };
        if obj_ptr.is_null() {
            continue;
        }
        // SAFETY: `obj_ptr` is non-null and valid.
        let obj_type = unsafe { (*obj_ptr).r#type };
        match obj_type {
            DRM_MODE_OBJECT_CRTC => has_crtc = true,
            DRM_MODE_OBJECT_CONNECTOR => has_connector = true,
            DRM_MODE_OBJECT_PLANE => has_plane = true,
            _ => return -EINVAL,
        }
    }

    if count > 0 && (!has_crtc || !has_connector || (universal_planes && !has_plane)) {
        return -EINVAL;
    }
    0
}

/// Insert an object into the unpublished lease set without treating duplicates
/// as success. The caller destroys the whole set if any insertion fails.
///
/// # Safety
/// `leases` must be a live, exclusively owned IDR initialized by the caller.
unsafe fn insert_lease_object(leases: *mut Idr, object_id: u32) -> i32 {
    let Ok(start) = i32::try_from(object_id) else {
        return -EINVAL;
    };
    let Some(end) = start.checked_add(1) else {
        return -EINVAL;
    };
    let marker = core::ptr::addr_of!(LEASE_OBJECT_TOKEN).cast_mut().cast();
    // SAFETY: The caller owns the unpublished IDR, and the non-null static
    // marker outlives all leases. The checked range selects exactly this ID.
    unsafe { idr_alloc(leases, marker, start, end) }
}

/// Resolves mode object IDs, checks object type constraints, and records the
/// explicit and implicit IDs into `leases`. Publication rechecks availability.
///
/// # Safety
/// `dev` and `lessor_priv` must stay live for this call, and `leases` must be
/// exclusively owned and initialized. `object_ids` must point to
/// `object_count` valid, kernel-owned `u32` entries.
unsafe fn populate_lease_set(
    dev: *mut DrmDevice,
    lessor_priv: *mut DrmFile,
    leases: *mut Idr,
    object_count: usize,
    object_ids: *const u32,
) -> i32 {
    let mut resolved_objects: alloc::vec::Vec<*mut DrmModeObject> = alloc::vec::Vec::new();
    // Reserve before acquiring object references, so every later push fits
    // and allocation failure follows the caller's unpublished-lease cleanup.
    if resolved_objects.try_reserve_exact(object_count).is_err() {
        return -ENOMEM;
    }
    // SAFETY: The live lessor file owns this atomic. This single snapshot
    // selects both validation and implicit-plane expansion for this request;
    // it does not publish other state, so relaxed ordering is sufficient.
    let universal_planes = unsafe { (*lessor_priv).universal_planes.load(Ordering::Relaxed) };

    // SAFETY: Resolves each requested object ID, enforces lease rules, and populates `leases`.
    unsafe {
        for idx in 0..object_count {
            let obj_id = *object_ids.add(idx);
            let obj = drm_mode_object_find(dev, lessor_priv, obj_id, 0);
            if obj.is_null() {
                for &held in &resolved_objects {
                    drm_mode_object_put(held);
                }
                return -ENOENT;
            }

            resolved_objects.push(obj);
        }

        let validation =
            validate_lease_objects(object_count, resolved_objects.as_ptr(), universal_planes);
        if validation != 0 {
            for &held in &resolved_objects {
                drm_mode_object_put(held);
            }
            return validation;
        }

        let result = (|| {
            for &object in &resolved_objects {
                let ret = insert_lease_object(leases, (*object).id);
                if ret < 0 {
                    return ret;
                }
                if universal_planes || (*object).r#type != DRM_MODE_OBJECT_CRTC {
                    continue;
                }
                // `object` is the base of a referenced CRTC. Its primary and
                // optional cursor planes are fixed while the device is live.
                let crtc = object
                    .cast::<u8>()
                    .sub(core::mem::offset_of!(DrmCrtc, base))
                    .cast::<DrmCrtc>();
                if (*crtc).primary.is_null() {
                    return -EINVAL;
                }
                for plane in [(*crtc).primary, (*crtc).cursor] {
                    if !plane.is_null() {
                        let ret = insert_lease_object(leases, (*plane).base.id);
                        if ret < 0 {
                            return ret;
                        }
                    }
                }
            }
            0
        })();

        for &held in &resolved_objects {
            drm_mode_object_put(held);
        }

        result
    }
}

/// Handler for the DRM_IOCTL_MODE_CREATE_LEASE ioctl.
#[unsafe(no_mangle)]
/// # Safety
/// `dev`, `data`, and `lessor_priv` must be non-null and point to valid instances of their respective types.
pub unsafe extern "C" fn drm_mode_create_lease_ioctl(
    dev: *mut DrmDevice,
    data: *mut core::ffi::c_void,
    lessor_priv: *mut DrmFile,
) -> i32 {
    let cl = data.cast::<DrmModeCreateLease>();
    // SAFETY: Checking device capabilities and parsing lease arguments.
    unsafe {
        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let flags = (*cl).flags;
        if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
            return -EINVAL;
        }

        let mut lessor = drm_file_get_master(lessor_priv);
        if lessor.is_null() {
            return -EINVAL;
        }

        // Only the top-level master can issue leases
        if !(*lessor).lessor.is_null() {
            drm_master_put(core::ptr::addr_of_mut!(lessor));
            return -EINVAL;
        }

        let object_count = (*cl).object_count as usize;
        let mut leases: Idr = core::mem::zeroed();
        idr_init(core::ptr::addr_of_mut!(leases));

        if object_count != 0 {
            let alloc_size = object_count
                .checked_mul(core::mem::size_of::<u32>())
                .unwrap_or(0);
            if alloc_size == 0 {
                idr_destroy(core::ptr::addr_of_mut!(leases));
                drm_master_put(core::ptr::addr_of_mut!(lessor));
                return -EINVAL;
            }

            let id_buf = kmalloc(alloc_size, GFP_KERNEL).cast::<u32>();
            if id_buf.is_null() {
                idr_destroy(core::ptr::addr_of_mut!(leases));
                drm_master_put(core::ptr::addr_of_mut!(lessor));
                return -ENOMEM;
            }

            if copy_from_user(
                id_buf.cast(),
                ((*cl).object_ids as usize) as *const core::ffi::c_void,
                alloc_size,
            ) != 0
            {
                kfree(id_buf.cast::<core::ffi::c_void>());
                idr_destroy(core::ptr::addr_of_mut!(leases));
                drm_master_put(core::ptr::addr_of_mut!(lessor));
                return -EFAULT;
            }

            let pop_res = populate_lease_set(
                dev,
                lessor_priv,
                core::ptr::addr_of_mut!(leases),
                object_count,
                id_buf,
            );
            kfree(id_buf.cast::<core::ffi::c_void>());

            if pop_res != 0 {
                idr_destroy(core::ptr::addr_of_mut!(leases));
                drm_master_put(core::ptr::addr_of_mut!(lessor));
                return pop_res;
            }
        }

        let lessee = create_lessee_master(lessor, core::ptr::addr_of_mut!(leases));
        if is_err(lessee) {
            idr_destroy(core::ptr::addr_of_mut!(leases));
            drm_master_put(core::ptr::addr_of_mut!(lessor));
            return ptr_err(lessee);
        }

        let lessee_id = (*lessee).lessee_id as u32;
        let fd = match LEASE_FILES.get() {
            Some(ops) => (ops.open_lessee_fd)(
                (*lessor_priv).filp,
                flags as c_int,
                adopt_lessee_master,
                lessee.cast::<c_void>(),
            ),
            None => -ENODEV,
        };
        if fd < 0 {
            // `setup` did not run, so the lessee reference is still ours;
            // dropping it tears the new lease down again.
            let mut lessee = lessee;
            drm_master_put(core::ptr::addr_of_mut!(lessee));
            drm_master_put(core::ptr::addr_of_mut!(lessor));
            return fd;
        }

        (*cl).lessee_id = lessee_id;
        (*cl).fd = fd as u32;

        drm_master_put(core::ptr::addr_of_mut!(lessor));
        0
    }
}

/// Compute a complete output word's user address without claiming that
/// independently mapped user pages belong to one Rust allocation. Only
/// `copy_to_user` may access the resulting address.
fn lease_output_address(base: u64, index: u32) -> Option<*mut c_void> {
    let offset = (index as usize).checked_mul(core::mem::size_of::<u32>())?;
    let address = (base as usize).checked_add(offset)?;
    address.checked_add(core::mem::size_of::<u32>())?;
    Some(address as *mut c_void)
}

/// Handler for the DRM_IOCTL_MODE_LIST_LESSEES ioctl.
#[unsafe(no_mangle)]
/// # Safety
/// `dev`, `data`, and `lessor_priv` must be non-null and point to valid instances of their respective types.
pub unsafe extern "C" fn drm_mode_list_lessees_ioctl(
    dev: *mut DrmDevice,
    data: *mut core::ffi::c_void,
    lessor_priv: *mut DrmFile,
) -> i32 {
    let arg = data.cast::<DrmModeListLessees>();
    // SAFETY: Checking modeset capability and querying lessees list.
    unsafe {
        if (*arg).pad != 0 {
            return -EINVAL;
        }
        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let mut lessor = drm_file_get_master(lessor_priv);
        if lessor.is_null() {
            return -EINVAL;
        }

        let mutex_ptr = core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex);
        mutex_lock(mutex_ptr);

        let max_entries = (*arg).count_lessees;
        let mut total_active = 0u32;
        let mut err = 0;

        for_each_lessee(lessor, |lessee| {
            let mut dummy = 0;
            let first_entry =
                idr_get_next(core::ptr::addr_of_mut!((*lessee).leases).cast(), &mut dummy);
            if !first_entry.is_null() {
                if total_active < max_entries {
                    let id_val = (*lessee).lessee_id as u32;
                    let Some(dest) = lease_output_address((*arg).lessees_ptr, total_active) else {
                        err = -EFAULT;
                        return false;
                    };
                    if copy_to_user(
                        dest,
                        core::ptr::addr_of!(id_val).cast(),
                        core::mem::size_of::<u32>(),
                    ) != 0
                    {
                        err = -EFAULT;
                        return false;
                    }
                }
                total_active += 1;
            }
            true
        });

        if err == 0 {
            (*arg).count_lessees = total_active;
        }
        mutex_unlock(mutex_ptr);
        drm_master_put(core::ptr::addr_of_mut!(lessor));
        err
    }
}

/// Handler for the DRM_IOCTL_MODE_GET_LEASE ioctl.
#[unsafe(no_mangle)]
/// # Safety
/// `dev`, `data`, and `lessee_priv` must be non-null and point to valid instances of their respective types.
pub unsafe extern "C" fn drm_mode_get_lease_ioctl(
    dev: *mut DrmDevice,
    data: *mut core::ffi::c_void,
    lessee_priv: *mut DrmFile,
) -> i32 {
    let arg = data.cast::<DrmModeGetLease>();
    // SAFETY: Checking modeset capability and querying leased objects.
    unsafe {
        if (*arg).pad != 0 {
            return -EINVAL;
        }
        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let mut lessee = drm_file_get_master(lessee_priv);
        if lessee.is_null() {
            return -EINVAL;
        }

        let mutex_ptr = core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex);
        mutex_lock(mutex_ptr);

        let query_idr = if (*lessee).lessor.is_null() {
            core::ptr::addr_of_mut!((*dev).mode_config.object_idr)
        } else {
            core::ptr::addr_of_mut!((*lessee).leases)
        };

        let max_objects = (*arg).count_objects;
        let mut total_count = 0u32;
        let mut err = 0;

        let mut cursor = 0;
        loop {
            let entry = idr_get_next(query_idr.cast(), &mut cursor);
            if entry.is_null() {
                break;
            }

            if total_count < max_objects {
                let id_val = cursor as u32;
                let Some(dest) = lease_output_address((*arg).objects_ptr, total_count) else {
                    err = -EFAULT;
                    break;
                };
                if copy_to_user(
                    dest,
                    core::ptr::addr_of!(id_val).cast(),
                    core::mem::size_of::<u32>(),
                ) != 0
                {
                    err = -EFAULT;
                    break;
                }
            }

            total_count += 1;
            cursor += 1;
        }

        if err == 0 {
            (*arg).count_objects = total_count;
        }
        mutex_unlock(mutex_ptr);
        drm_master_put(core::ptr::addr_of_mut!(lessee));
        err
    }
}

/// Handler for the DRM_IOCTL_MODE_REVOKE_LEASE ioctl.
#[unsafe(no_mangle)]
/// # Safety
/// `dev`, `data`, and `lessor_priv` must be non-null and point to valid instances of their respective types.
pub unsafe extern "C" fn drm_mode_revoke_lease_ioctl(
    dev: *mut DrmDevice,
    data: *mut core::ffi::c_void,
    lessor_priv: *mut DrmFile,
) -> i32 {
    let arg = data.cast::<DrmModeRevokeLease>();
    // SAFETY: Checking modeset capability and revoking designated lessee.
    unsafe {
        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let mut lessor = drm_file_get_master(lessor_priv);
        if lessor.is_null() {
            return -EINVAL;
        }

        let mutex_ptr = core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex);
        mutex_lock(mutex_ptr);

        let target_lessee = find_lessee_master(lessor, (*arg).lessee_id as i32);
        if target_lessee.is_null() {
            mutex_unlock(mutex_ptr);
            drm_master_put(core::ptr::addr_of_mut!(lessor));
            return -ENOENT;
        }

        if (*target_lessee).lessor != lessor {
            mutex_unlock(mutex_ptr);
            drm_master_put(core::ptr::addr_of_mut!(lessor));
            return -EACCES;
        }

        revoke_lease_hierarchy(target_lessee);

        mutex_unlock(mutex_ptr);
        drm_master_put(core::ptr::addr_of_mut!(lessor));
        0
    }
}

#[cfg(test)]
mod tests {
    use drm_kapi::config::{EBUSY, is_err, ptr_err};

    use super::*;
    use crate::{
        drm_drv_types::DrmDriver,
        drm_drv_upstream::{drm_dev_alloc, drm_dev_put},
        drm_file_upstream::{drm_file_alloc, drm_file_free},
        drm_mode_object_upstream::{drm_mode_object_add, drm_mode_object_unregister},
    };

    #[test]
    fn test_lease_owner_and_held() {
        crate::test_support::init_runtime();
        // SAFETY: Test setup allocates a mock device and file for verification.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);
            let master = (*file_priv).master;

            assert_eq!(drm_lease_owner(master), master);
            assert!(drm_lease_held(file_priv, 42));
            assert!(_drm_lease_held(file_priv, 42));

            drm_file_free(file_priv);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn lease_publication_rechecks_pending_objects() {
        let mut fixture = crate::test_support::TestDrm::new(DRIVER_MODESET);
        fixture.init_registry();
        let dev = fixture.ptr();
        // SAFETY: The fixture owns the device and registry until all masters
        // and the stack-owned mode object have been removed below.
        unsafe {
            let mut master = drm_master_create(dev);
            let mut plane: DrmModeObject = core::mem::zeroed();
            assert_eq!(
                drm_mode_object_add(dev, &mut plane, DRM_MODE_OBJECT_PLANE),
                0
            );
            // Fail the real scratch Vec allocation after the fallible input
            // copy has been allocated. This stack file borrows our live master.
            let mut file: DrmFile = core::mem::zeroed();
            file.master = master;
            drm_kapi::locks::spin_lock_init(&mut file.master_lookup_lock);
            let objects = [plane.id; 3];
            let mut request = DrmModeCreateLease {
                object_ids: objects.as_ptr() as u64,
                object_count: objects.len() as u32,
                flags: 0,
                lessee_id: u32::MAX,
                fd: u32::MAX,
            };
            let refs = drm_kapi::memory::kref_read(&(*master).refcount);
            let (result, rejected, balance) = crate::test_support::with_allocation_failure(
                core::alloc::Layout::array::<*mut DrmModeObject>(objects.len()).unwrap(),
                || {
                    drm_mode_create_lease_ioctl(
                        dev,
                        (&mut request as *mut DrmModeCreateLease).cast(),
                        &mut file,
                    )
                },
            );
            assert!(rejected, "the scratch allocation must actually fail");
            assert_eq!(result, -ENOMEM);
            assert_eq!(balance, 0, "the copied input buffer must be freed");
            assert_eq!(drm_kapi::memory::kref_read(&(*master).refcount), refs);
            assert_eq!(request.lessee_id, u32::MAX);
            assert_eq!(request.fd, u32::MAX);
            assert!(drm_kapi::idr::idr_is_empty(&(*master).lessee_idr));

            let mut first: Idr = core::mem::zeroed();
            let mut pending: Idr = core::mem::zeroed();
            idr_init(&mut first);
            idr_init(&mut pending);
            assert!(insert_lease_object(&mut first, plane.id) >= 0);
            assert!(insert_lease_object(&mut pending, plane.id) >= 0);

            // Both requests built their unpublished object sets before either
            // committed. The second publisher must recheck the final set,
            // including planes added implicitly during request preparation.
            let mut winner = create_lessee_master(master, &mut first);
            assert!(!winner.is_null() && !is_err(winner));
            let loser = create_lessee_master(master, &mut pending);
            assert!(is_err(loser));
            assert_eq!(ptr_err(loser), -EBUSY);
            assert!(master_holds_object(winner, plane.id as i32));
            assert!(!idr_find(&mut pending, plane.id as core::ffi::c_ulong).is_null());

            drm_master_put(&mut winner);
            let mut retry = create_lessee_master(master, &mut pending);
            assert!(!retry.is_null() && !is_err(retry));
            drm_master_put(&mut retry);
            idr_destroy(&mut first);
            idr_destroy(&mut pending);
            drm_mode_object_unregister(dev, &mut plane);

            drm_master_put(&mut master);
            idr_destroy(core::ptr::addr_of_mut!((*dev).mode_config.object_idr));
        }
    }

    #[test]
    fn test_null_pointers_and_boundary_conditions() {
        assert_eq!(
            lease_output_address(4092, 2),
            Some(4100usize as *mut c_void)
        );
        assert!(lease_output_address(u64::MAX - 2, 0).is_none());
        assert!(lease_output_address(u64::MAX - 7, 2).is_none());
        // SAFETY: Testing null handling across lease interfaces.
        unsafe {
            assert!(drm_lease_owner(core::ptr::null_mut()).is_null());
            assert!(_drm_lease_held(core::ptr::null_mut(), 100));
            assert!(drm_lease_held(core::ptr::null_mut(), 100));
            assert_eq!(
                drm_lease_filter_crtcs(core::ptr::null_mut(), 0b1101),
                0b1101
            );
            drm_lease_destroy(core::ptr::null_mut());
            drm_lease_revoke(core::ptr::null_mut());
        }
    }

    #[test]
    fn test_lease_owner_hierarchy() {
        // SAFETY: Testing synthetic hierarchy of DrmMaster pointers without mock hardware.
        unsafe {
            let mut root: DrmMaster = core::mem::zeroed();
            let mut mid: DrmMaster = core::mem::zeroed();
            let mut leaf: DrmMaster = core::mem::zeroed();

            mid.lessor = core::ptr::addr_of_mut!(root);
            leaf.lessor = core::ptr::addr_of_mut!(mid);

            assert_eq!(
                drm_lease_owner(core::ptr::addr_of_mut!(root)),
                core::ptr::addr_of_mut!(root)
            );
            assert_eq!(
                drm_lease_owner(core::ptr::addr_of_mut!(mid)),
                core::ptr::addr_of_mut!(root)
            );
            assert_eq!(
                drm_lease_owner(core::ptr::addr_of_mut!(leaf)),
                core::ptr::addr_of_mut!(root)
            );
        }
    }
}
