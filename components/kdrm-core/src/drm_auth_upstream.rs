// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_auth.c`.
//!
//! Master and authentication management for DRM devices. Implements master
//! creation/destruction, current-master checks, magic generation and authentication,
//! and the SET_MASTER / DROP_MASTER ioctls.

#![allow(non_snake_case)]

use alloc::boxed::Box;

#[cfg(not(test))]
use drm_kapi::mutex::{mutex_lock, mutex_unlock};
use drm_kapi::{
    assert::warn_on,
    config::{CAP_SYS_ADMIN, EBUSY, EINVAL, ENOMEM},
    kernel_services::capable,
    list::INIT_LIST_HEAD,
    locks::{spin_lock_raw, spin_unlock_raw},
    memory::{kfree, kref_init, kref_put},
};
/// # Safety
/// Test mock function; caller must adhere to mutex locking protocol.
#[cfg(test)]
unsafe fn mutex_lock<M>(_mutex: M) {}
/// # Safety
/// Test mock function; caller must adhere to mutex unlocking protocol.
#[cfg(test)]
unsafe fn mutex_unlock<M>(_mutex: M) {}
use drm_kapi::types::Kref;

use crate::{
    drm_auth_types::DrmMaster,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_lease_upstream::{drm_lease_destroy, drm_lease_owner, drm_lease_revoke},
    drm_mode_object_upstream::{
        DRIVER_MODESET, drm_core_check_feature, idr_alloc, idr_destroy, idr_find, idr_init,
        idr_init_base, idr_remove, idr_replace,
    },
    uapi_drm_types::DrmAuth,
};

const EACCES: i32 = 13;

// upstream: drivers/gpu/drm/drm_auth.c drm_is_current_master_locked()
/// # Safety
/// `fpriv` must be non-null, properly aligned, and point to a live `DrmFile` whose
/// `master_lookup_lock` or device `master_mutex` is held by the caller.
unsafe fn drm_is_current_master_locked(fpriv: *mut DrmFile) -> bool {
    // SAFETY: The caller holds the appropriate lookup/device lock and guarantees `fpriv`,
    // its `minor`, and `dev` remain live and readable without concurrent modification.
    unsafe {
        (*fpriv).is_master
            && !(*fpriv).master.is_null()
            && drm_lease_owner((*fpriv).master) == (*(*fpriv).minor).dev.as_ref().unwrap().master
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_is_current_master()
#[unsafe(no_mangle)]
/// # Safety
/// If `fpriv` is non-null, it must point to a live, properly aligned `DrmFile`
/// with an initialized `master_lookup_lock`.
pub unsafe extern "C" fn drm_is_current_master(fpriv: *mut DrmFile) -> bool {
    if fpriv.is_null() {
        return false;
    }
    // SAFETY: `fpriv` is non-null. `master_lookup_lock` is acquired and released around
    // the call to `drm_is_current_master_locked` to protect reading `(*fpriv).master`.
    unsafe {
        spin_lock_raw(core::ptr::addr_of_mut!((*fpriv).master_lookup_lock));
        let ret = drm_is_current_master_locked(fpriv);
        spin_unlock_raw(core::ptr::addr_of_mut!((*fpriv).master_lookup_lock));
        ret
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_getmagic()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to a live `DrmDevice` with initialized `master_mutex`.
/// `data` must point to a valid, writable `DrmAuth` structure.
/// `file_priv` must point to a live `DrmFile` opened on `dev`.
pub unsafe extern "C" fn drm_getmagic(
    dev: *mut DrmDevice,
    data: *mut core::ffi::c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let auth = data.cast::<DrmAuth>();
    let mut ret = 0;

    // SAFETY: `dev` and its `master_mutex` are live. Caller guarantees `data` points to writable `DrmAuth`
    // and `file_priv` is a live file on `dev`. `master_mutex` serializes magic allocation in `master.magic_map`.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        if (*file_priv).magic == 0 {
            ret = idr_alloc(
                core::ptr::addr_of_mut!((*(*file_priv).master).magic_map),
                file_priv.cast(),
                1,
                0,
            );
            if ret >= 0 {
                (*file_priv).magic = ret as u32;
            }
        }
        (*auth).magic = (*file_priv).magic;
        mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
    }

    if ret < 0 { ret } else { 0 }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_authmagic()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to a live `DrmDevice` with initialized `master_mutex`.
/// `data` must point to a valid, readable `DrmAuth` structure.
/// `file_priv` must point to a live `DrmFile` opened on `dev`.
pub unsafe extern "C" fn drm_authmagic(
    dev: *mut DrmDevice,
    data: *mut core::ffi::c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let auth = data.cast::<DrmAuth>();

    // SAFETY: `dev` and its `master_mutex` are live. `file_priv` and `auth` are valid per caller contract.
    // `master_mutex` protects searching and replacing the entry in `(*(*file_priv).master).magic_map`.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        let file = idr_find(
            core::ptr::addr_of_mut!((*(*file_priv).master).magic_map),
            (*auth).magic as core::ffi::c_ulong,
        )
        .cast::<DrmFile>();
        if !file.is_null() {
            (*file).authenticated = true;
            idr_replace(
                core::ptr::addr_of_mut!((*(*file_priv).master).magic_map),
                core::ptr::null_mut(),
                (*auth).magic as i32,
            );
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));

        if !file.is_null() { 0 } else { -EINVAL }
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_create()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to a live `DrmDevice` that remains valid throughout the lifetime of the created master.
pub unsafe extern "C" fn drm_master_create(dev: *mut DrmDevice) -> *mut DrmMaster {
    // SAFETY: Zeroed allocation produces an unaliased, initialized `DrmMaster` on the heap.
    let master = Box::into_raw(Box::new(unsafe { core::mem::zeroed::<DrmMaster>() }));

    // SAFETY: `master` is a newly allocated non-null pointer. Initializing refcount, IDRs, and lists
    // sets up all invariants expected by the DRM master and lease subsystems.
    unsafe {
        (*master).dev = dev;
        kref_init(core::ptr::addr_of_mut!((*master).refcount));
        idr_init_base(core::ptr::addr_of_mut!((*master).magic_map), 1);
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*master).lessees));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*master).lessee_list));
        idr_init(core::ptr::addr_of_mut!((*master).leases));
        idr_init_base(core::ptr::addr_of_mut!((*master).lessee_idr), 1);
    }

    master
}

// upstream: drivers/gpu/drm/drm_auth.c drm_set_master()
/// # Safety
/// Caller must hold `(*dev).master_mutex`. `dev` and `fpriv` must point to live `DrmDevice`
/// and `DrmFile` structures respectively.
unsafe fn drm_set_master(dev: *mut DrmDevice, fpriv: *mut DrmFile, new_master: bool) {
    // SAFETY: `dev` and `fpriv` are live and `master_mutex` is held by caller.
    // `(*dev).master` receives an acquired reference to `(*fpriv).master`.
    unsafe {
        (*dev).master = drm_master_get((*fpriv).master);
        if let Some(driver) = (*dev).driver.as_ref()
            && let Some(master_set) = driver.master_set
        {
            master_set(dev, fpriv, new_master);
        }
        (*fpriv).was_master = true;
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_new_set_master()
/// # Safety
/// Caller must hold `(*dev).master_mutex`. `dev` and `fpriv` must point to live `DrmDevice`
/// and `DrmFile` structures respectively.
unsafe fn drm_new_set_master(dev: *mut DrmDevice, fpriv: *mut DrmFile) -> i32 {
    // SAFETY: Caller holds `master_mutex`. A new master is created and assigned to `fpriv` under
    // `master_lookup_lock`, and previous master (if any) is released.
    unsafe {
        warn_on((*fpriv).is_master);
        let mut old_master = (*fpriv).master;
        let new_master = drm_master_create(dev);
        if new_master.is_null() {
            return -ENOMEM;
        }
        spin_lock_raw(core::ptr::addr_of_mut!((*fpriv).master_lookup_lock));
        (*fpriv).master = new_master;
        spin_unlock_raw(core::ptr::addr_of_mut!((*fpriv).master_lookup_lock));

        (*fpriv).is_master = true;
        (*fpriv).authenticated = true;

        drm_set_master(dev, fpriv, true);

        if !old_master.is_null() {
            drm_master_put(core::ptr::addr_of_mut!(old_master));
        }

        0
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_check_perm()
/// # Safety
/// `file_priv` must point to a live, properly aligned `DrmFile`.
unsafe fn drm_master_check_perm(_dev: *mut DrmDevice, file_priv: *mut DrmFile) -> i32 {
    // SAFETY: `file_priv` is live; reading `was_master` is safe.
    unsafe {
        if (*file_priv).was_master {
            return 0;
        }
        if !capable(CAP_SYS_ADMIN) {
            return -EACCES;
        }
        0
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_setmaster_ioctl()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to a live `DrmDevice`. `file_priv` must point to a live `DrmFile` opened on `dev`.
pub unsafe extern "C" fn drm_setmaster_ioctl(
    dev: *mut DrmDevice,
    _data: *mut core::ffi::c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` and its `master_mutex` are live. The caller guarantees `file_priv` is an open file on `dev`.
    // Mutex acquisition synchronizes checking and assigning master role across concurrent ioctls.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        let ret = drm_master_check_perm(dev, file_priv);
        if ret != 0 {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return ret;
        }

        if drm_is_current_master_locked(file_priv) {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return 0;
        }

        if !(*dev).master.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return -EBUSY;
        }

        if (*file_priv).master.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return -EINVAL;
        }

        if !(*file_priv).is_master {
            let res = drm_new_set_master(dev, file_priv);
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return res;
        }

        if !(*(*file_priv).master).lessor.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return -EINVAL;
        }

        drm_set_master(dev, file_priv, false);
        mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
        0
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_drop_master()
/// # Safety
/// Caller must hold `(*dev).master_mutex`. `dev` and `fpriv` must point to live `DrmDevice` and `DrmFile`.
unsafe fn drm_drop_master(dev: *mut DrmDevice, fpriv: *mut DrmFile) {
    // SAFETY: Caller holds `master_mutex`. Invokes driver master_drop hook if registered,
    // then decrements refcount on device master.
    unsafe {
        if let Some(driver) = (*dev).driver.as_ref()
            && let Some(master_drop) = driver.master_drop
        {
            master_drop(dev, fpriv);
        }
        drm_master_put(core::ptr::addr_of_mut!((*dev).master));
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_dropmaster_ioctl()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to a live `DrmDevice`. `file_priv` must point to a live `DrmFile` opened on `dev`.
pub unsafe extern "C" fn drm_dropmaster_ioctl(
    dev: *mut DrmDevice,
    _data: *mut core::ffi::c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` and `file_priv` are live per caller contract. `master_mutex` serializes
    // checking and dropping master status.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        let ret = drm_master_check_perm(dev, file_priv);
        if ret != 0 {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return ret;
        }

        if !drm_is_current_master_locked(file_priv) {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return -EINVAL;
        }

        if (*dev).master.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return -EINVAL;
        }

        if !(*(*file_priv).master).lessor.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return -EINVAL;
        }

        drm_drop_master(dev, file_priv);
        mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
        0
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_open()
#[unsafe(no_mangle)]
/// # Safety
/// `file_priv` must point to a live `DrmFile` whose `minor` and underlying `dev` are initialized.
pub unsafe extern "C" fn drm_master_open(file_priv: *mut DrmFile) -> i32 {
    // SAFETY: `file_priv` and `(*(*file_priv).minor).dev` are live. Acquiring `master_mutex`
    // coordinates initial master assignment or reference retrieval.
    unsafe {
        let dev = (*(*file_priv).minor).dev;
        let mut ret = 0;
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        if (*dev).master.is_null() {
            ret = drm_new_set_master(dev, file_priv);
        } else {
            spin_lock_raw(core::ptr::addr_of_mut!((*file_priv).master_lookup_lock));
            (*file_priv).master = drm_master_get((*dev).master);
            spin_unlock_raw(core::ptr::addr_of_mut!((*file_priv).master_lookup_lock));
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
        ret
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_release()
#[unsafe(no_mangle)]
/// # Safety
/// `file_priv` must point to a live `DrmFile` being closed.
pub unsafe extern "C" fn drm_master_release(file_priv: *mut DrmFile) {
    // SAFETY: `file_priv` is being released. Holding `master_mutex` allows removing magic entries,
    // dropping master if current, revoking leases, and dropping the file master reference.
    unsafe {
        let dev = (*(*file_priv).minor).dev;
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        let master = (*file_priv).master;
        if (*file_priv).magic != 0 && !master.is_null() {
            idr_remove(
                core::ptr::addr_of_mut!((*master).magic_map),
                (*file_priv).magic as i32,
            );
        }

        if drm_is_current_master_locked(file_priv)
            && !(*dev).master.is_null()
            && (*dev).master == (*file_priv).master
        {
            drm_drop_master(dev, file_priv);
        }

        if drm_core_check_feature(dev, DRIVER_MODESET)
            && (*file_priv).is_master
            && !master.is_null()
        {
            drm_lease_revoke(master);
        }

        if !(*file_priv).master.is_null() {
            drm_master_put(core::ptr::addr_of_mut!((*file_priv).master));
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_get()
#[unsafe(no_mangle)]
/// # Safety
/// If `master` is non-null, it must point to a live `DrmMaster` with an initialized `refcount`.
pub unsafe extern "C" fn drm_master_get(master: *mut DrmMaster) -> *mut DrmMaster {
    if !master.is_null() {
        // SAFETY: `master` is checked non-null and caller guarantees its `refcount` is initialized.
        unsafe {
            drm_kapi::memory::kref_get(core::ptr::addr_of_mut!((*master).refcount));
        }
    }
    master
}

// upstream: drivers/gpu/drm/drm_auth.c drm_file_get_master()
#[unsafe(no_mangle)]
/// # Safety
/// If `file_priv` is non-null, it must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_file_get_master(file_priv: *mut DrmFile) -> *mut DrmMaster {
    if file_priv.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `file_priv` is non-null. `master_lookup_lock` protects reading `(*file_priv).master`
    // and taking a reference via `drm_master_get`.
    unsafe {
        spin_lock_raw(core::ptr::addr_of_mut!((*file_priv).master_lookup_lock));
        let master = if !(*file_priv).master.is_null() {
            drm_master_get((*file_priv).master)
        } else {
            core::ptr::null_mut()
        };
        spin_unlock_raw(core::ptr::addr_of_mut!((*file_priv).master_lookup_lock));
        master
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_destroy()
/// # Safety
/// `kref` must point to the `refcount` field of a `DrmMaster` allocated via `Box::into_raw`
/// whose reference count has reached zero.
unsafe extern "C" fn drm_master_destroy(kref: *mut Kref) {
    let master = kref.cast::<DrmMaster>();
    // SAFETY: The kref count reached zero with no remaining references. Destroys leases and IDRs,
    // frees `unique` string if allocated, and reclaims memory via `Box::from_raw(master)`.
    unsafe {
        let dev = (*master).dev;
        if drm_core_check_feature(dev, DRIVER_MODESET) {
            drm_lease_destroy(master);
        }
        idr_destroy(core::ptr::addr_of_mut!((*master).magic_map));
        idr_destroy(core::ptr::addr_of_mut!((*master).leases));
        idr_destroy(core::ptr::addr_of_mut!((*master).lessee_idr));

        if !(*master).unique.is_null() {
            kfree((*master).unique);
        }
        drop(Box::from_raw(master));
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_put()
#[unsafe(no_mangle)]
/// # Safety
/// If `master` and `*master` are non-null, `*master` must point to a live `DrmMaster`
/// with initialized `refcount`.
pub unsafe extern "C" fn drm_master_put(master: *mut *mut DrmMaster) {
    // SAFETY: Reads `*master` and verifies it is non-null before invoking `kref_put`.
    if !master.is_null() && !unsafe { (*master).is_null() } {
        // SAFETY: `*master` is verified non-null and valid for refcount decrement.
        // It is nulled out immediately to prevent dangling pointer reuse.
        unsafe {
            kref_put(
                core::ptr::addr_of_mut!((**master).refcount),
                drm_master_destroy,
            );
            *master = core::ptr::null_mut();
        }
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_internal_acquire()
#[unsafe(no_mangle)]
/// # Safety
/// If `dev` is non-null, it must point to a live `DrmDevice` with initialized `master_mutex`.
pub unsafe extern "C" fn drm_master_internal_acquire(dev: *mut DrmDevice) -> bool {
    if dev.is_null() {
        return false;
    }
    // SAFETY: `dev` is non-null and `master_mutex` is initialized. Locks `master_mutex`
    // and checks whether `(*dev).master` is null.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        if !(*dev).master.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return false;
        }
        true
    }
}

// upstream: drivers/gpu/drm/drm_auth.c drm_master_internal_release()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to a live `DrmDevice` whose `master_mutex` is held by the caller.
pub unsafe extern "C" fn drm_master_internal_release(dev: *mut DrmDevice) {
    if !dev.is_null() {
        // SAFETY: `dev` is non-null; unlocks `master_mutex` held from `drm_master_internal_acquire`.
        unsafe {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        drm_drv_types::DrmDriver,
        drm_drv_upstream::{drm_dev_alloc, drm_dev_put},
        drm_file_upstream::{drm_file_alloc, drm_file_free},
    };

    #[test]
    fn test_master_set_drop_and_magic() {
        crate::test_support::init_runtime();
        // SAFETY: Test exercises master set/drop and magic authentication with mock driver, dev, and files.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            assert!(!dev.is_null());

            // 1. First file opened becomes master (drm_open_helper() -> drm_master_open())
            let file1 = drm_file_alloc((*dev).primary);
            assert!(!file1.is_null());
            assert_eq!(super::drm_master_open(file1), 0);
            assert!(drm_is_current_master(file1));
            assert!((*file1).authenticated);

            // 2. Second file opened is not master, and not authenticated
            let file2 = drm_file_alloc((*dev).primary);
            assert!(!file2.is_null());
            assert_eq!(super::drm_master_open(file2), 0);
            assert!(!drm_is_current_master(file2));
            assert!(!(*file2).authenticated);

            // 3. Magic authentication
            let mut auth_req = DrmAuth { magic: 0 };
            assert_eq!(
                drm_getmagic(dev, &mut auth_req as *mut _ as *mut _, file2),
                0
            );
            assert!(auth_req.magic > 0);

            // Master (file1) authenticates file2's magic
            assert_eq!(
                drm_authmagic(dev, &mut auth_req as *mut _ as *mut _, file1),
                0
            );
            assert!((*file2).authenticated);

            // Re-authenticating same magic returns -EINVAL (already consumed)
            assert_eq!(
                drm_authmagic(dev, &mut auth_req as *mut _ as *mut _, file1),
                -EINVAL
            );

            // 4. Drop master
            // file2 is not master and lacks CAP_SYS_ADMIN; dropping fails with -EACCES
            assert_eq!(
                drm_dropmaster_ioctl(dev, core::ptr::null_mut(), file2),
                -EACCES
            );

            // file1 drops master
            assert_eq!(drm_dropmaster_ioctl(dev, core::ptr::null_mut(), file1), 0);
            assert!(!drm_is_current_master(file1));
            assert!((*dev).master.is_null());

            // 5. Set master
            // file1 was master, so it can reclaim master via setmaster_ioctl
            assert_eq!(drm_setmaster_ioctl(dev, core::ptr::null_mut(), file1), 0);
            assert!(drm_is_current_master(file1));

            // file2 lacks permission so it cannot set master (-EACCES)
            assert_eq!(
                drm_setmaster_ioctl(dev, core::ptr::null_mut(), file2),
                -EACCES
            );

            // With was_master set, file2 has permission but fails because file1 is already master (-EBUSY)
            (*file2).was_master = true;
            assert_eq!(
                drm_setmaster_ioctl(dev, core::ptr::null_mut(), file2),
                -EBUSY
            );

            drm_file_free(file2);
            drm_file_free(file1);
            drm_dev_put(dev);
        }
    }
}
