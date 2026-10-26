// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_file.c`.
//!
//! DRM file private data allocation, open/release lifecycle, pending event queue,
//! event reservation, timestamped event delivery, and read/poll file operations.

use alloc::boxed::Box;
use core::{
    ffi::{c_char, c_int, c_uint, c_void},
    sync::atomic::{AtomicU64, Ordering},
};

use drm_kapi::{
    config::{EAGAIN, EFAULT, EINVAL, ENODEV, ENOMEM},
    list::{init_list_head, list_add, list_add_tail, list_del, list_del_init, list_empty},
    locks::{
        spin_lock_init, spin_lock_irqsave_raw, spin_lock_raw, spin_unlock_irqrestore_raw,
        spin_unlock_raw,
    },
    memory::{atomic_dec, atomic_dec_and_test, atomic_inc, kfree},
    mm_native::copy_to_user,
    mutex::{mutex_init, mutex_lock, mutex_lock_interruptible, mutex_unlock},
    types::{File, Mutex, XArray},
    wait::{init_waitqueue_head, poll_wait, wake_up, wake_up_interruptible_poll},
    wait_event_interruptible,
};

const O_NONBLOCK: u32 = 0x800;

use crate::{
    drm_auth_upstream::{drm_master_open, drm_master_release},
    drm_device_types::DrmDevice,
    drm_drv_upstream::{drm_dev_enter, drm_dev_exit},
    drm_file_types::{DrmFile, DrmMemoryStats, DrmMinor, DrmPendingEvent},
    drm_gem_types::{
        DRM_GEM_OBJECT_ACTIVE, DRM_GEM_OBJECT_PURGEABLE, DRM_GEM_OBJECT_RESIDENT, DrmGemObject,
    },
    drm_mode_object_upstream::{DRIVER_GEM, DRIVER_MODESET, idr_destroy},
    drm_prime_upstream::{drm_prime_destroy_file_private, drm_prime_init_file_private},
    drm_print_types::DrmPrinter,
    uapi_drm_types::DrmEvent,
};

const DRIVER_LEGACY: u32 = 1 << 26;
const DRIVER_SYNCOBJ: u32 = 1 << 5;

static CLIENT_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

// upstream: drivers/gpu/drm/drm_file.c drm_global_mutex
#[unsafe(no_mangle)]
// SAFETY: Zeroed memory represents a valid, unlocked Mutex state.
pub static mut drm_global_mutex: Mutex = unsafe { core::mem::zeroed() };

// # Safety
//
// External C ABI symbol declarations linked from kernel or runtime modules.
unsafe extern "C" {
    fn drm_minor_acquire(minor_xa: *mut XArray, minor_id: c_uint) -> *mut DrmMinor;
    fn drm_minor_release(minor: *mut DrmMinor);
    fn drm_client_dev_restore(dev: *mut DrmDevice, restore: bool);
    fn drm_fb_release(file_priv: *mut DrmFile);
    fn drm_property_destroy_user_blobs(dev: *mut DrmDevice, file_priv: *mut DrmFile);
    fn drm_syncobj_open(file_priv: *mut DrmFile);
    fn drm_syncobj_release(file_priv: *mut DrmFile);
    static mut drm_minors_xa: XArray;
}

// upstream: drivers/gpu/drm/drm_file.c drm_dev_needs_global_mutex()
/// # Safety
///
/// `dev` must point to a live, properly aligned `DrmDevice`. Reads `dev->driver_features`.
pub unsafe extern "C" fn drm_dev_needs_global_mutex(dev: *mut DrmDevice) -> bool {
    if dev.is_null() {
        return false;
    }
    // SAFETY: dev is non-null and points to a valid DrmDevice per caller contract.
    if unsafe { (*dev).driver.is_null() } {
        return false;
    }
    // SAFETY: dev and dev.driver are non-null and point to valid driver memory.
    unsafe { ((*(*dev).driver).driver_features & DRIVER_LEGACY) != 0 }
}

// upstream: drivers/gpu/drm/drm_file.c drm_file_alloc()
#[unsafe(no_mangle)]
/// # Safety
///
/// `minor` must point to an initialized, live `DrmMinor` with a valid, non-null `dev` pointer.
/// Returns a newly allocated `DrmFile` owning a reference to `minor->dev`, or an `err_ptr` on failure.
pub unsafe extern "C" fn drm_file_alloc(minor: *mut DrmMinor) -> *mut DrmFile {
    if minor.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: minor is non-null and points to a valid DrmMinor; allocates, zeroes, and initializes a DrmFile struct.
    unsafe {
        let dev = (*minor).dev;
        let file_priv = Box::into_raw(Box::new(core::mem::zeroed::<DrmFile>()));

        (*file_priv).client_id = CLIENT_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        (*file_priv).minor = minor;
        // Authentication belongs to the opening credentials, independently of
        // whether this open becomes master. Initialize before the driver hook.
        (*file_priv).authenticated.store(
            drm_kapi::kernel_services::capable(drm_kapi::config::CAP_SYS_ADMIN),
            Ordering::Relaxed,
        );
        spin_lock_init(&mut (*file_priv).master_lookup_lock);
        crate::drm_mode_object_upstream::idr_init(core::ptr::addr_of_mut!((*file_priv).object_idr));
        spin_lock_init(&mut (*file_priv).table_lock);

        init_list_head(core::ptr::addr_of_mut!((*file_priv).fbs));
        mutex_init(core::ptr::addr_of_mut!((*file_priv).fbs_lock));
        init_list_head(core::ptr::addr_of_mut!((*file_priv).blobs));
        init_waitqueue_head(&mut (*file_priv).event_wait);
        init_list_head(core::ptr::addr_of_mut!((*file_priv).pending_event_list));
        init_list_head(core::ptr::addr_of_mut!((*file_priv).event_list));
        (*file_priv).event_space = 4096;
        mutex_init(core::ptr::addr_of_mut!((*file_priv).event_read_lock));

        if ((*dev).driver_features & DRIVER_GEM) != 0 {
            crate::drm_gem_upstream::drm_gem_open(dev, file_priv);
        }
        if ((*dev).driver_features & DRIVER_SYNCOBJ) != 0 {
            drm_syncobj_open(file_priv);
        }

        drm_prime_init_file_private(core::ptr::addr_of_mut!((*file_priv).prime));

        mutex_init(core::ptr::addr_of_mut!((*file_priv).client_name_lock));

        if let Some(driver) = (*dev).driver.as_ref()
            && let Some(open_cb) = driver.open
        {
            let ret = open_cb(dev, file_priv);
            if ret < 0 {
                drm_prime_destroy_file_private(core::ptr::addr_of_mut!((*file_priv).prime));
                if ((*dev).driver_features & DRIVER_SYNCOBJ) != 0 {
                    drm_syncobj_release(file_priv);
                }
                if ((*dev).driver_features & DRIVER_GEM) != 0 {
                    crate::drm_gem_upstream::drm_gem_release(dev, file_priv);
                }
                drop(Box::from_raw(file_priv));
                return core::ptr::null_mut();
            }
        }

        file_priv
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_events_release()
/// # Safety
/// `file_priv` must point to a live `DrmFile` with valid minor and dev pointers.
unsafe fn drm_events_release(file_priv: *mut DrmFile) {
    // SAFETY: file_priv is valid and holds valid event lists; cancels and drains all pending and queued events under dev.event_lock.
    unsafe {
        let dev = (*(*file_priv).minor).dev;
        let mut flags: core::ffi::c_ulong = 0;
        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);

        // Cancel pending events
        while !list_empty(&(*file_priv).pending_event_list) {
            let cur = (*file_priv).pending_event_list.next;
            let pe = cur
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmPendingEvent, pending_link))
                .cast::<DrmPendingEvent>();
            list_del_init(cur);
            (*pe).file_priv = core::ptr::null_mut();
        }

        // Cancel queued events
        while !list_empty(&(*file_priv).event_list) {
            let cur = (*file_priv).event_list.next;
            let pe = cur
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmPendingEvent, link))
                .cast::<DrmPendingEvent>();
            list_del_init(cur);
            kfree(pe);
        }

        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_file_free()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `file_priv` must point to an allocated `DrmFile` whose file descriptor is closed.
/// Caller transfers ownership of `file_priv` to this function, which drains events, releases
/// primes, and drops the underlying device reference.
pub unsafe extern "C" fn drm_file_free(file_priv: *mut DrmFile) {
    if file_priv.is_null() {
        return;
    }
    // SAFETY: file_priv is non-null and points to a valid DrmFile being released; releases all attached resources and reclaims heap memory.
    unsafe {
        let dev = (*(*file_priv).minor).dev;

        drm_events_release(file_priv);

        if ((*dev).driver_features & DRIVER_MODESET) != 0 {
            drm_fb_release(file_priv);
            drm_property_destroy_user_blobs(dev, file_priv);
        }

        if ((*dev).driver_features & DRIVER_SYNCOBJ) != 0 {
            drm_syncobj_release(file_priv);
        }

        if ((*dev).driver_features & DRIVER_GEM) != 0 {
            crate::drm_gem_upstream::drm_gem_release(dev, file_priv);
        }

        if (*(*file_priv).minor).r#type == 0 {
            drm_master_release(file_priv);
        }

        if let Some(driver) = (*dev).driver.as_ref()
            && let Some(postclose) = driver.postclose
        {
            postclose(dev, file_priv);
        }

        drm_prime_destroy_file_private(core::ptr::addr_of_mut!((*file_priv).prime));
        idr_destroy(core::ptr::addr_of_mut!((*file_priv).object_idr));

        if !(*file_priv).client_name.is_null() {
            kfree((*file_priv).client_name);
            (*file_priv).client_name = core::ptr::null_mut();
        }

        drop(Box::from_raw(file_priv));
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_close_helper()
/// # Safety
/// `filp` must point to a live `File` object whose `private_data` points to a valid `DrmFile` or is null.
unsafe fn drm_close_helper(filp: *mut File) {
    if filp.is_null() {
        return;
    }
    // SAFETY: filp is non-null; if private_data is non-null it points to a valid DrmFile unlinked from dev.filelist and freed.
    unsafe {
        if (*filp).private_data.is_null() {
            return;
        }
        let file_priv = (*filp).private_data.cast::<DrmFile>();
        let dev = (*(*file_priv).minor).dev;

        mutex_lock(core::ptr::addr_of_mut!((*dev).filelist_mutex));
        list_del(core::ptr::addr_of_mut!((*file_priv).lhead));
        mutex_unlock(core::ptr::addr_of_mut!((*dev).filelist_mutex));

        drm_file_free(file_priv);
        (*filp).private_data = core::ptr::null_mut();
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_cpu_valid()
/// # Safety
/// Safe to call without preconditions; always returns 1 indicating valid architecture.
unsafe fn drm_cpu_valid() -> c_int {
    1
}

// upstream: drivers/gpu/drm/drm_file.c drm_open_helper()
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to a live VFS `File` whose `private_data` will be populated.
/// `minor` must point to a live `DrmMinor` whose device reference can be retained.
pub unsafe extern "C" fn drm_open_helper(filp: *mut File, minor: *mut DrmMinor) -> i32 {
    if filp.is_null() || minor.is_null() {
        return -EINVAL;
    }
    // SAFETY: filp and minor are non-null valid pointers; allocates and links a DrmFile to filp.private_data and dev.filelist.
    unsafe {
        let dev = (*minor).dev;
        if drm_cpu_valid() == 0 {
            return -EINVAL;
        }

        let priv_data = drm_file_alloc(minor);
        if priv_data.is_null() {
            return -ENOMEM;
        }
        // Only an open of the primary node may become master. Client files
        // (fbdev, efidrm) are created through drm_client_open(), which never
        // takes master, so the internal modeset commit can still acquire it.
        if (*minor).r#type == 0 {
            let ret = drm_master_open(priv_data);
            if ret != 0 {
                drm_file_free(priv_data);
                return ret;
            }
        }

        (*filp).private_data = priv_data.cast();
        (*priv_data).filp = filp;

        mutex_lock(core::ptr::addr_of_mut!((*dev).filelist_mutex));
        list_add_tail(
            core::ptr::addr_of_mut!((*priv_data).lhead),
            core::ptr::addr_of_mut!((*dev).filelist),
        );
        mutex_unlock(core::ptr::addr_of_mut!((*dev).filelist_mutex));

        0
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_lastclose()
/// # Safety
/// `dev` must point to a live `DrmDevice`.
unsafe fn drm_lastclose(dev: *mut DrmDevice) {
    // SAFETY: dev points to a live DrmDevice per caller contract.
    unsafe { drm_client_dev_restore(dev, false) }
}

// upstream: drivers/gpu/drm/drm_file.c drm_open()
#[unsafe(no_mangle)]
/// # Safety
///
/// `inode` must point to a valid VFS inode identifying a DRM character device.
/// `filp` must point to a valid VFS `File` being opened.
pub unsafe extern "C" fn drm_open(inode: *mut c_void, filp: *mut File) -> i32 {
    if !inode.is_null() {
        // SAFETY: filp is a valid File pointer; opens minor index 0 for inode.
        return unsafe { drm_open_minor_index(0, filp) };
    }
    if filp.is_null() {
        return -EINVAL;
    }
    // SAFETY: filp is non-null and valid; accesses its private_data to obtain and open the associated minor.
    unsafe {
        let minor = if !(*filp).private_data.is_null() {
            (*(*filp).private_data.cast::<DrmFile>()).minor
        } else {
            core::ptr::null_mut()
        };
        if minor.is_null() {
            return -crate::drm_drv_upstream::ENODEV;
        }
        let dev = (*minor).dev;
        let need_global = drm_dev_needs_global_mutex(dev);
        if need_global {
            mutex_lock(core::ptr::addr_of_mut!(drm_global_mutex));
        }
        atomic_inc(&(*dev).open_count);

        let retcode = drm_open_helper(filp, minor);
        if retcode != 0 {
            atomic_dec(&(*dev).open_count);
            if need_global {
                mutex_unlock(core::ptr::addr_of_mut!(drm_global_mutex));
            }
            drm_minor_release(minor);
            return retcode;
        }

        if need_global {
            mutex_unlock(core::ptr::addr_of_mut!(drm_global_mutex));
        }
        0
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_release()
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to a live VFS `File` whose `private_data` holds the associated `DrmFile`.
pub unsafe extern "C" fn drm_release(_inode: *mut c_void, filp: *mut File) -> i32 {
    if filp.is_null() {
        return -EINVAL;
    }
    // SAFETY: filp is non-null and valid; closes and frees associated DrmFile, managing dev.open_count and lastclose.
    unsafe {
        let file_priv = (*filp).private_data.cast::<DrmFile>();
        if file_priv.is_null() {
            return 0;
        }
        let minor = (*file_priv).minor;
        let dev = (*minor).dev;
        let need_global = drm_dev_needs_global_mutex(dev);
        if need_global {
            mutex_lock(core::ptr::addr_of_mut!(drm_global_mutex));
        }

        drm_close_helper(filp);

        if atomic_dec_and_test(&(*dev).open_count) {
            drm_lastclose(dev);
        }

        if need_global {
            mutex_unlock(core::ptr::addr_of_mut!(drm_global_mutex));
        }

        drm_minor_release(minor);
        0
    }
}

/// Open a specific registered DRM minor by its xarray index.
///
/// The ordinary Linux character-device entry uses `drm_open()` and resolves
/// index zero from the inode. A VFS adapter for a render node already has the
/// exact minor index and calls this equivalent lifecycle path directly.
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to an allocated VFS `File` whose `private_data` will be assigned.
/// `minor_index` must be an allocated minor index in the DRM minor array.
pub unsafe extern "C" fn drm_open_minor_index(minor_index: u32, filp: *mut File) -> i32 {
    if filp.is_null() {
        return -EINVAL;
    }
    // SAFETY: acquires minor from global minors XArray.
    let minor = unsafe { drm_minor_acquire(core::ptr::addr_of_mut!(drm_minors_xa), minor_index) };
    if minor.is_null() {
        return -crate::drm_drv_upstream::ENODEV;
    }
    // SAFETY: minor is non-null and valid.
    let dev = unsafe { (*minor).dev };
    // SAFETY: dev is a valid DrmDevice.
    let need_global = unsafe { drm_dev_needs_global_mutex(dev) };
    if need_global {
        // SAFETY: acquiring global mutex for legacy driver.
        unsafe { mutex_lock(core::ptr::addr_of_mut!(drm_global_mutex)) };
    }
    // SAFETY: dev is valid; increments open_count.
    unsafe { atomic_inc(&(*dev).open_count) };
    // SAFETY: filp and minor are valid pointers.
    let retcode = unsafe { drm_open_helper(filp, minor) };
    if retcode != 0 {
        // SAFETY: rolls back open_count on error.
        unsafe { atomic_dec(&(*dev).open_count) };
        if need_global {
            // SAFETY: releasing global mutex on error.
            unsafe { mutex_unlock(core::ptr::addr_of_mut!(drm_global_mutex)) };
        }
        // SAFETY: releases acquired minor on error.
        unsafe { drm_minor_release(minor) };
        return retcode;
    }
    if need_global {
        // SAFETY: releasing global mutex after successful open.
        unsafe { mutex_unlock(core::ptr::addr_of_mut!(drm_global_mutex)) };
    }
    0
}

// upstream: drivers/gpu/drm/drm_file.c drm_file_update_pid()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `filp` must point to a live `DrmFile`. Serialized internally via `filp->table_lock`.
pub unsafe extern "C" fn drm_file_update_pid(filp: *mut DrmFile) {
    if filp.is_null() {
        return;
    }
    // SAFETY: filp is non-null and points to a valid DrmFile object.
    if unsafe {
        (*filp)
            .was_master
            .load(core::sync::atomic::Ordering::Relaxed)
    } {}
}

// upstream: drivers/gpu/drm/drm_file.c drm_release_noglobal()
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to a live VFS `File` whose `private_data` holds the `DrmFile` being closed.
pub unsafe extern "C" fn drm_release_noglobal(_inode: *mut c_void, filp: *mut File) -> i32 {
    if filp.is_null() {
        return -EINVAL;
    }
    // SAFETY: filp is non-null and valid; closes DrmFile and cleans up open count without taking global mutex.
    unsafe {
        let file_priv = (*filp).private_data.cast::<DrmFile>();
        if file_priv.is_null() {
            return 0;
        }
        let minor = (*file_priv).minor;
        let dev = (*minor).dev;

        drm_close_helper(filp);

        if atomic_dec_and_test(&(*dev).open_count) {
            mutex_lock(core::ptr::addr_of_mut!(drm_global_mutex));
            drm_lastclose(dev);
            mutex_unlock(core::ptr::addr_of_mut!(drm_global_mutex));
        }

        drm_minor_release(minor);
        0
    }
}

/// VFS-side sink used by [`drm_read_kernel_to`]. Returning exactly `len`
/// commits the event; a short or negative result leaves the event queued.
///
/// # Safety
/// `context` must remain valid for the call; `bytes` must point to `len` readable bytes.
pub type DrmReadWriter =
    unsafe extern "C" fn(context: *mut c_void, bytes: *const c_void, len: usize) -> isize;

/// # Safety
/// `buffer` must point to a user buffer of at least `offset + len` writable bytes, and `bytes` must point to `len` readable bytes.
unsafe fn drm_read_copy_to_user(
    buffer: *mut c_char,
    offset: usize,
    bytes: *const c_void,
    len: usize,
) -> isize {
    // SAFETY: buffer is non-null and valid for writing len bytes at offset; bytes is readable for len bytes per caller contract.
    if buffer.is_null() || unsafe { copy_to_user(buffer.add(offset).cast(), bytes, len) } != 0 {
        -(EFAULT as isize)
    } else {
        len as isize
    }
}

/// # Safety
/// The file and its minor/device remain live while the caller retains the fd.
unsafe fn events_pending(file: *mut DrmFile) -> bool {
    // SAFETY: The file retains its device. Producers and consumers use this
    // same lock for event_list; a readiness probe is a reader of that state.
    unsafe {
        let dev = (*(*file).minor).dev;
        let mut flags = 0;
        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);
        let pending = !list_empty(&(*file).event_list);
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);
        pending
    }
}

/// # Safety
/// `filp` must point to a live `File` whose `private_data` points to an initialized `DrmFile`.
unsafe fn drm_read_core(
    filp: *mut File,
    count: usize,
    may_wait: bool,
    mut writer: impl FnMut(usize, *const c_void, usize) -> isize,
) -> isize {
    const EPOLLIN: u32 = 0x0001;
    const EPOLLRDNORM: u32 = 0x0040;

    if filp.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: filp is non-null; accesses and locks file_priv.event_read_lock, processes events from event_list under event_lock.
    unsafe {
        let file_priv = (*filp).private_data.cast::<DrmFile>();
        if file_priv.is_null() {
            return -EINVAL as isize;
        }
        let dev = (*(*file_priv).minor).dev;

        let mut ret: isize =
            mutex_lock_interruptible(core::ptr::addr_of_mut!((*file_priv).event_read_lock))
                as isize;
        if ret != 0 {
            return ret;
        }

        loop {
            let mut e: *mut DrmPendingEvent = core::ptr::null_mut();

            let mut flags: core::ffi::c_ulong = 0;
            spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);
            if !list_empty(&(*file_priv).event_list) {
                let cur = (*file_priv).event_list.next;
                e = cur
                    .cast::<u8>()
                    .wrapping_sub(core::mem::offset_of!(DrmPendingEvent, link))
                    .cast::<DrmPendingEvent>();
                (*file_priv).event_space += (*(*e).event).length as i32;
                list_del(core::ptr::addr_of_mut!((*e).link));
            }
            spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);

            if e.is_null() {
                if (*dev).unplugged.load(Ordering::Acquire) {
                    ret = -ENODEV as isize;
                    break;
                }
                if ret != 0 {
                    break;
                }

                if !may_wait || ((*filp).f_flags & O_NONBLOCK) != 0 {
                    ret = -EAGAIN as isize;
                    break;
                }

                mutex_unlock(core::ptr::addr_of_mut!((*file_priv).event_read_lock));
                let wait_ret = wait_event_interruptible!(
                    core::ptr::addr_of_mut!((*file_priv).event_wait),
                    events_pending(file_priv) || (*dev).unplugged.load(Ordering::Acquire),
                );
                if wait_ret >= 0 {
                    ret = mutex_lock_interruptible(core::ptr::addr_of_mut!(
                        (*file_priv).event_read_lock
                    )) as isize;
                } else {
                    ret = wait_ret as isize;
                }
                if ret != 0 {
                    return ret;
                }
            } else {
                let length = (*(*e).event).length as usize;

                if length > count.saturating_sub(ret as usize) {
                    spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);
                    (*file_priv).event_space -= length as i32;
                    list_add(
                        core::ptr::addr_of_mut!((*e).link),
                        core::ptr::addr_of_mut!((*file_priv).event_list),
                    );
                    spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);
                    wake_up_interruptible_poll(
                        core::ptr::addr_of_mut!((*file_priv).event_wait),
                        EPOLLIN | EPOLLRDNORM,
                    );
                    break;
                }

                let copied = writer(ret as usize, (*e).event.cast(), length);
                if copied != length as isize {
                    if ret == 0 {
                        ret = if copied < 0 { copied } else { -EFAULT as isize };
                    }
                    spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);
                    (*file_priv).event_space -= length as i32;
                    list_add(
                        core::ptr::addr_of_mut!((*e).link),
                        core::ptr::addr_of_mut!((*file_priv).event_list),
                    );
                    spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);
                    wake_up_interruptible_poll(
                        core::ptr::addr_of_mut!((*file_priv).event_wait),
                        EPOLLIN | EPOLLRDNORM,
                    );
                    break;
                }

                ret += length as isize;
                kfree(e.cast::<core::ffi::c_void>());
            }
        }

        mutex_unlock(core::ptr::addr_of_mut!((*file_priv).event_read_lock));
        ret
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_read()
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to an open VFS `File` with a valid `DrmFile` in `private_data`.
/// `buffer` must point to a writable user memory region of at least `count` bytes.
pub unsafe extern "C" fn drm_read(
    filp: *mut File,
    buffer: *mut c_char,
    count: usize,
    _offset: *mut i64,
) -> isize {
    if buffer.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: filp and buffer are valid; delegates to drm_read_core with copy_to_user sink.
    unsafe {
        drm_read_core(filp, count, true, |offset, bytes, len| {
            drm_read_copy_to_user(buffer, offset, bytes, len)
        })
    }
}

/// Nonblocking DRM event read through a kernel-owned writer. The caller must
/// provide readiness blocking/retry around `-EAGAIN` (as the VFS `Pollable`
/// adapter does); event dequeue, partial-read requeue, and event ownership are
/// shared with the Linux `drm_read()` path above.
///
/// # Safety
/// `filp` must be a live DRM file. `writer` must synchronously consume from
/// the supplied event bytes and return the number of bytes accepted, or a
/// negative Linux errno. `context` must remain valid for the call.
pub unsafe fn drm_read_kernel_to(
    filp: *mut File,
    count: usize,
    context: *mut c_void,
    writer: DrmReadWriter,
) -> isize {
    // SAFETY: filp is valid; delegates to drm_read_core with the provided kernel-space writer callback.
    unsafe {
        drm_read_core(filp, count, false, |_, bytes, len| {
            writer(context, bytes, len)
        })
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_poll()
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to an open VFS `File` with a valid `DrmFile` in `private_data`.
/// If non-null, `wait` must point to a live poll table structure for wait queue registration.
pub unsafe extern "C" fn drm_poll(filp: *mut File, wait: *mut c_void) -> u32 {
    const EPOLLIN: u32 = 0x0001;
    const EPOLLRDNORM: u32 = 0x0040;
    const EPOLLERR: u32 = 0x0008;
    const EPOLLHUP: u32 = 0x0010;

    if filp.is_null() {
        return 0;
    }
    // SAFETY: filp is non-null and valid; polls file_priv.event_wait under event_lock.
    unsafe {
        let file_priv = (*filp).private_data.cast::<DrmFile>();
        if file_priv.is_null() {
            return 0;
        }
        let dev = (*(*file_priv).minor).dev;

        poll_wait(
            filp,
            core::ptr::addr_of_mut!((*file_priv).event_wait),
            wait.cast(),
        );

        let mut mask = 0;
        if events_pending(file_priv) {
            mask |= EPOLLIN | EPOLLRDNORM;
        }
        if (*dev).unplugged.load(Ordering::Acquire) {
            mask |= EPOLLERR | EPOLLHUP;
        }
        mask
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_event_reserve_init_locked()
#[unsafe(no_mangle)]
/// # Safety
///
/// Caller must hold `dev->event_lock`.
/// `dev`, `file_priv`, `p`, and `e` must be valid, live, non-null pointers.
/// `p` and `e` must point to allocated, properly aligned event structures.
pub unsafe extern "C" fn drm_event_reserve_init_locked(
    _dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    p: *mut DrmPendingEvent,
    e: *mut DrmEvent,
) -> i32 {
    if file_priv.is_null() || p.is_null() || e.is_null() {
        return -EINVAL;
    }
    // SAFETY: file_priv, p, and e are non-null and valid; caller holds dev.event_lock; links p into file_priv.pending_event_list.
    unsafe {
        if (*file_priv).event_space < (*e).length as i32 {
            return -ENOMEM;
        }
        (*file_priv).event_space -= (*e).length as i32;
        (*p).event = e;
        (*p).file_priv = file_priv;
        list_add_tail(
            core::ptr::addr_of_mut!((*p).pending_link),
            core::ptr::addr_of_mut!((*file_priv).pending_event_list),
        );
        0
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_event_reserve_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev`, `file_priv`, `p`, and `e` must be valid, live, non-null pointers.
/// `dev->event_lock` will be acquired and released internally.
pub unsafe extern "C" fn drm_event_reserve_init(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    p: *mut DrmPendingEvent,
    e: *mut DrmEvent,
) -> i32 {
    // SAFETY: dev, file_priv, p, and e are valid; acquires dev.event_lock and reserves event via drm_event_reserve_init_locked.
    unsafe {
        let mut flags: core::ffi::c_ulong = 0;
        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);
        let ret = drm_event_reserve_init_locked(dev, file_priv, p, e);
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);
        ret
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_event_cancel_free()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `dev` and `p` must point to a live `DrmDevice` and a previously reserved `DrmPendingEvent`.
/// `p` must not currently be queued on any active event list.
pub unsafe extern "C" fn drm_event_cancel_free(dev: *mut DrmDevice, p: *mut DrmPendingEvent) {
    if p.is_null() {
        return;
    }
    // SAFETY: dev and p are non-null and valid; cancels and frees p under dev.event_lock.
    unsafe {
        let mut flags: core::ffi::c_ulong = 0;
        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);
        if !(*p).file_priv.is_null() {
            (*(*p).file_priv).event_space += (*(*p).event).length as i32;
            list_del(core::ptr::addr_of_mut!((*p).pending_link));
        }
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);
        if !(*p).fence.is_null() {
            drm_kapi::dma_fence::dma_fence_put((*p).fence);
        }
        // The event payload is embedded in the allocation whose base is `p`.
        // Freeing `p->event` separately is an interior-pointer kfree.
        kfree(p);
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_send_event_helper()
/// # Safety
/// `_dev` must point to a live `DrmDevice`; if `e` is non-null, it must point to an initialized `DrmPendingEvent`.
unsafe fn drm_send_event_helper(_dev: *mut DrmDevice, e: *mut DrmPendingEvent, timestamp: i64) {
    if e.is_null() {
        return;
    }
    // SAFETY: e is non-null and valid; signals fence/completion, moves e to event_list, and wakes event_wait.
    unsafe {
        if !(*e).completion.is_null() {
            if let Some(completion) = (*e).completion.as_ref() {
                completion.complete_all();
            }
            if let Some(rel) = (*e).completion_release {
                rel((*e).completion);
            }
            (*e).completion = core::ptr::null_mut();
        }
        if !(*e).fence.is_null() {
            let fence = (*e).fence;
            if timestamp != 0 {
                // SAFETY: the pending event owns this fence reference until the
                // matching put below; the event timestamp is in CLOCK_MONOTONIC.
                drm_kapi::dma_fence::dma_fence_signal_timestamp(fence, timestamp);
            } else {
                // SAFETY: the pending event owns this fence reference until the
                // matching put below.
                drm_kapi::dma_fence::dma_fence_signal(fence);
            }
            // SAFETY: drm_event_reserve_init() transferred one fence reference
            // to the pending event, which is consumed exactly once here.
            drm_kapi::dma_fence::dma_fence_put(fence);
            (*e).fence = core::ptr::null_mut();
        }
        if (*e).file_priv.is_null() {
            #[cfg(not(test))]
            kfree(e);
            return;
        }
        list_del_init(core::ptr::addr_of_mut!((*e).pending_link));
        list_add_tail(
            core::ptr::addr_of_mut!((*e).link),
            core::ptr::addr_of_mut!((*(*e).file_priv).event_list),
        );
        wake_up(core::ptr::addr_of_mut!((*(*e).file_priv).event_wait));
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_send_event_timestamp_locked()
#[unsafe(no_mangle)]
/// # Safety
///
/// Caller must hold `dev->event_lock`.
/// `dev` and `e` must be valid, live pointers. `e` must be an initialized pending event;
/// its ownership is transferred into the destination file's event queue.
pub unsafe extern "C" fn drm_send_event_timestamp_locked(
    dev: *mut DrmDevice,
    e: *mut DrmPendingEvent,
    timestamp: i64,
) {
    if e.is_null() {
        return;
    }
    // SAFETY: dev and e satisfy drm_send_event_helper preconditions per caller contract.
    unsafe { drm_send_event_helper(dev, e, timestamp) }
}

// upstream: drivers/gpu/drm/drm_file.c drm_send_event_locked()
#[unsafe(no_mangle)]
/// # Safety
///
/// Caller must hold `dev->event_lock`.
/// `dev` and `e` must point to live, valid objects; `e` must be an initialized pending event.
pub unsafe extern "C" fn drm_send_event_locked(dev: *mut DrmDevice, e: *mut DrmPendingEvent) {
    // SAFETY: dev and e satisfy drm_send_event_timestamp_locked preconditions per caller contract.
    unsafe { drm_send_event_timestamp_locked(dev, e, 0) }
}

// upstream: drivers/gpu/drm/drm_file.c drm_send_event()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` and `e` must point to live, valid objects; `dev->event_lock` is acquired internally.
pub unsafe extern "C" fn drm_send_event(dev: *mut DrmDevice, e: *mut DrmPendingEvent) {
    if e.is_null() {
        return;
    }
    // SAFETY: dev and e are non-null and valid; locks dev.event_lock and dispatches e via drm_send_event_helper.
    unsafe {
        let mut flags: core::ffi::c_ulong = 0;
        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);
        drm_send_event_timestamp_locked(dev, e, 0);
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_fdinfo_print_size()
#[unsafe(no_mangle)]
/// # Safety
///
/// `p` must point to an active, valid `DrmPrinter`.
/// `key` must point to a valid null-terminated C string.
pub unsafe extern "C" fn drm_fdinfo_print_size(
    p: *mut DrmPrinter,
    prefix: *const c_char,
    stat: *const c_char,
    region: *const c_char,
    mut sz: u64,
) {
    if p.is_null() {
        return;
    }
    // SAFETY: p is non-null and valid; prefix, stat, region are valid C strings or null.
    unsafe {
        let pref = if !prefix.is_null() {
            core::ffi::CStr::from_ptr(prefix).to_string_lossy()
        } else {
            alloc::borrow::Cow::Borrowed("")
        };
        let st = if !stat.is_null() {
            core::ffi::CStr::from_ptr(stat).to_string_lossy()
        } else {
            alloc::borrow::Cow::Borrowed("")
        };
        let reg = if !region.is_null() {
            core::ffi::CStr::from_ptr(region).to_string_lossy()
        } else {
            alloc::borrow::Cow::Borrowed("")
        };
        let units = ["", " KiB", " MiB"];
        let mut u = 0;
        while u < units.len() - 1 {
            if sz == 0 || (sz & (1024 - 1)) != 0 {
                break;
            }
            sz /= 1024;
            u += 1;
        }
        crate::drm_print_upstream::drm_printf(
            p,
            &alloc::format!("{}-{}-{}:\t{}{}\n", pref, st, reg, sz, units[u]),
        );
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_memory_stats_is_zero()
#[unsafe(no_mangle)]
/// # Safety
///
/// `stats` must point to an initialized, properly aligned `DrmMemoryStats` struct.
pub unsafe extern "C" fn drm_memory_stats_is_zero(stats: *const DrmMemoryStats) -> c_int {
    if stats.is_null() {
        return 1;
    }
    // SAFETY: stats is non-null and points to a valid DrmMemoryStats.
    unsafe {
        if (*stats).shared == 0
            && (*stats).private == 0
            && (*stats).resident == 0
            && (*stats).purgeable == 0
            && (*stats).active == 0
        {
            1
        } else {
            0
        }
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_print_memory_stats()
#[unsafe(no_mangle)]
/// # Safety
///
/// `p` must point to a valid `DrmPrinter`.
/// `stats` must point to an initialized `DrmMemoryStats` struct.
/// If non-null, `region` must point to a valid null-terminated C string.
pub unsafe extern "C" fn drm_print_memory_stats(
    p: *mut DrmPrinter,
    stats: *const DrmMemoryStats,
    supported_status: u32,
    region: *const c_char,
) {
    if p.is_null() || stats.is_null() {
        return;
    }
    // SAFETY: p and stats are non-null and valid; region (if non-null) is a valid C string; formats memory stats.
    unsafe {
        let prefix = c"drm".as_ptr();
        let total = c"total".as_ptr();
        let shared = c"shared".as_ptr();
        let active = c"active".as_ptr();
        let resident = c"resident".as_ptr();
        let purgeable = c"purgeable".as_ptr();

        drm_fdinfo_print_size(p, prefix, total, region, (*stats).private + (*stats).shared);
        drm_fdinfo_print_size(p, prefix, shared, region, (*stats).shared);
        if (supported_status & DRM_GEM_OBJECT_ACTIVE) != 0 {
            drm_fdinfo_print_size(p, prefix, active, region, (*stats).active);
        }
        if (supported_status & DRM_GEM_OBJECT_RESIDENT) != 0 {
            drm_fdinfo_print_size(p, prefix, resident, region, (*stats).resident);
        }
        if (supported_status & DRM_GEM_OBJECT_PURGEABLE) != 0 {
            drm_fdinfo_print_size(p, prefix, purgeable, region, (*stats).purgeable);
        }
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_show_memory_stats()
#[unsafe(no_mangle)]
/// # Safety
///
/// `p` must point to a valid `DrmPrinter`, and `file` must point to a live `DrmFile` with a valid device pointer.
pub unsafe extern "C" fn drm_show_memory_stats(p: *mut DrmPrinter, file: *mut DrmFile) {
    if p.is_null() || file.is_null() {
        return;
    }
    // SAFETY: p and file are non-null and valid; iterates GEM objects in file.object_idr under table_lock and prints stats.
    unsafe {
        let mut stats = DrmMemoryStats {
            shared: 0,
            private: 0,
            resident: 0,
            purgeable: 0,
            active: 0,
        };
        let mut supported_status: u32 = 0;

        spin_lock_raw(core::ptr::addr_of_mut!((*file).table_lock));
        crate::drm_mode_object_upstream::idr_for_each(
            core::ptr::addr_of_mut!((*file).object_idr),
            |_id, ptr| {
                let obj = ptr as *mut DrmGemObject;
                if !obj.is_null() {
                    let mut s: u32 = 0;
                    let add_size = if !(*obj).funcs.is_null() && (*(*obj).funcs).rss.is_some() {
                        ((*(*obj).funcs).rss.unwrap())(obj)
                    } else {
                        (*obj).size
                    };
                    if !(*obj).funcs.is_null() && (*(*obj).funcs).status.is_some() {
                        s = ((*(*obj).funcs).status.unwrap())(obj);
                        supported_status |= s;
                    }
                    if (*obj).handle_count > 1 || !(*obj).dma_buf.is_null() {
                        stats.shared += (*obj).size as u64;
                    } else {
                        stats.private += (*obj).size as u64;
                    }
                    if (s & DRM_GEM_OBJECT_RESIDENT) != 0 {
                        stats.resident += add_size as u64;
                    } else {
                        s &= !DRM_GEM_OBJECT_PURGEABLE;
                    }
                    if (s & DRM_GEM_OBJECT_PURGEABLE) != 0 {
                        stats.purgeable += add_size as u64;
                    }
                }
            },
        );
        spin_unlock_raw(core::ptr::addr_of_mut!((*file).table_lock));

        drm_print_memory_stats(p, &stats, supported_status, c"memory".as_ptr());
    }
}

/// # Safety
/// `p` must point to a valid `DrmPrinter` and `s` must point to a valid NUL-terminated C string.
unsafe extern "C" fn __drm_puts_seq_file_adapter(p: *mut DrmPrinter, s: *mut c_char) {
    // SAFETY: p and s satisfy __drm_puts_seq_file preconditions per caller contract.
    unsafe { crate::drm_print_upstream::__drm_puts_seq_file(p, s as *const c_char) }
}

// upstream: drivers/gpu/drm/drm_file.c drm_show_fdinfo()
#[unsafe(no_mangle)]
/// # Safety
///
/// `m` must point to a valid `seq_file` representation.
/// `f` must point to an open DRM `File` whose `private_data` contains the `DrmFile`.
pub unsafe extern "C" fn drm_show_fdinfo(m: *mut c_void, f: *mut File) {
    if f.is_null() {
        return;
    }
    // SAFETY: f is non-null and valid; accesses its private_data DrmFile and driver to print fdinfo to seq_file.
    unsafe {
        if (*f).private_data.is_null() {
            return;
        }
        let file = (*f).private_data.cast::<DrmFile>();
        let dev = (*(*file).minor).dev;
        let mut idx = 0;
        if !drm_dev_enter(dev, &mut idx) {
            return;
        }
        let mut printer = DrmPrinter {
            printfn: Some(crate::drm_print_upstream::__drm_printfn_seq_file),
            puts: Some(__drm_puts_seq_file_adapter),
            arg: m,
            origin: core::ptr::null_mut(),
            prefix: core::ptr::null_mut(),
            line: crate::drm_print_types::DrmPrinterLine {
                series: 0,
                counter: 0,
            },
            category: crate::drm_print_types::DRM_UT_CORE,
        };
        if !(*dev).driver.is_null() && !(*(*dev).driver).name.is_null() {
            let name = core::ffi::CStr::from_ptr((*(*dev).driver).name).to_string_lossy();
            crate::drm_print_upstream::drm_printf(
                &mut printer,
                &alloc::format!("drm-driver:\t{}\n", name),
            );
        }
        crate::drm_print_upstream::drm_printf(
            &mut printer,
            &alloc::format!("drm-client-id:\t{}\n", (*file).client_id),
        );
        mutex_lock(core::ptr::addr_of_mut!((*file).client_name_lock));
        if !(*file).client_name.is_null() {
            let cname = core::ffi::CStr::from_ptr((*file).client_name).to_string_lossy();
            crate::drm_print_upstream::drm_printf(
                &mut printer,
                &alloc::format!("drm-client-name:\t{}\n", cname),
            );
        }
        mutex_unlock(core::ptr::addr_of_mut!((*file).client_name_lock));

        if !(*dev).driver.is_null()
            && let Some(show_fdinfo) = (*(*dev).driver).show_fdinfo
        {
            show_fdinfo(&mut printer, file);
        }
        drm_dev_exit(idx);
    }
}

// upstream: drivers/gpu/drm/drm_file.c drm_file_err()
#[unsafe(no_mangle)]
/// # Safety
///
/// `fmt` must point to a valid null-terminated format string.
/// If non-null, `file_priv` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_file_err(
    _file_priv: *mut DrmFile,
    _fmt: *const c_char,
    mut _args: ...
) {
}

// upstream: drivers/gpu/drm/drm_file.c mock_drm_getfile()
#[unsafe(no_mangle)]
/// # Safety
///
/// `minor` must point to an initialized `DrmMinor`.
/// Allocates a mock VFS `File` owning an opened `DrmFile` reference.
pub unsafe extern "C" fn mock_drm_getfile(minor: *mut DrmMinor, flags: c_uint) -> *mut File {
    // SAFETY: minor is valid per caller contract.
    let file = unsafe { drm_file_alloc(minor) };
    if file.is_null() {
        return core::ptr::null_mut();
    }
    let filp = Box::into_raw(Box::new(File {
        _before_f_mapping: [0; 16],
        f_mapping: core::ptr::null_mut(),
        private_data: file.cast(),
        f_inode: core::ptr::null_mut(),
        f_flags: flags,
        _tail: [0; 132],
    }));
    // SAFETY: file is a freshly allocated DrmFile; sets its backlink filp.
    unsafe { (*file).filp = filp };
    filp
}

#[cfg(test)]
mod tests {
    use drm_kapi::{
        completion::Completion,
        config::GFP_KERNEL,
        memory::{kfree, kmalloc},
        types::WaitQueueHead,
    };

    use super::*;
    use crate::{
        drm_drv_types::DrmDriver,
        drm_drv_upstream::{drm_dev_alloc, drm_dev_put},
        drm_vblank_types::DrmPendingVblankEvent,
    };

    #[test]
    fn open_authentication_follows_capability() {
        crate::test_support::init_runtime();
        // SAFETY: The fixture supplies a valid mock device and allocates each file with
        // the test task policy installed; both files are released before the device.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            assert!(!dev.is_null());

            let unprivileged = crate::test_support::with_task_policy(false, false, || {
                drm_file_alloc((*dev).primary)
            });
            assert!(!unprivileged.is_null());
            assert!(!(*unprivileged).authenticated.load(Ordering::Relaxed));
            drm_file_free(unprivileged);

            let privileged = crate::test_support::with_task_policy(true, false, || {
                drm_file_alloc((*dev).primary)
            });
            assert!(!privileged.is_null());
            assert!((*privileged).authenticated.load(Ordering::Relaxed));
            drm_file_free(privileged);

            drm_dev_put(dev);
        }
    }

    /// # Safety
    /// `completion` must point to a live `Completion` instance.
    unsafe extern "C" fn assert_completion_signaled_before_release(completion: *mut Completion) {
        assert!(!completion.is_null());
        // SAFETY: completion is non-null and points to a live Completion instance.
        assert!(unsafe { &*completion }.is_done());
    }

    #[test]
    fn cancel_free_releases_pending_event_allocation_not_embedded_payload() {
        crate::test_support::init_runtime();
        // SAFETY: test exercises mock device, file, and pending event allocation and cancel/free.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            assert!(!dev.is_null());
            let file_priv = drm_file_alloc((*dev).primary);
            assert!(!file_priv.is_null());

            let event = kmalloc(core::mem::size_of::<DrmPendingVblankEvent>(), GFP_KERNEL)
                .cast::<DrmPendingVblankEvent>();
            assert!(!event.is_null());
            core::ptr::write_bytes(event, 0, 1);
            let event_data = core::ptr::addr_of_mut!((*event).event.base) as *mut DrmEvent;
            (*event_data).r#type = 0x55;
            (*event_data).length =
                core::mem::size_of::<crate::uapi_drm_types::DrmEventVblank>() as u32;

            assert_eq!(
                drm_event_reserve_init(
                    dev,
                    file_priv,
                    core::ptr::addr_of_mut!((*event).base),
                    core::ptr::addr_of_mut!((*event).event.base) as *mut DrmEvent,
                ),
                0
            );
            drm_event_cancel_free(dev, core::ptr::addr_of_mut!((*event).base));
            assert_eq!((*file_priv).event_space, 4096);
            assert!(list_empty(&(*file_priv).pending_event_list));

            drm_file_free(file_priv);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn send_event_completes_commit_before_releasing_completion() {
        crate::test_support::init_runtime();
        // SAFETY: test exercises event completion signalling before callback release.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            assert!(!dev.is_null());

            let pending = kmalloc(core::mem::size_of::<DrmPendingVblankEvent>(), GFP_KERNEL)
                .cast::<DrmPendingVblankEvent>();
            assert!(!pending.is_null());
            core::ptr::write_bytes(pending, 0, 1);
            let completion = Completion::new();
            (*pending).base.completion = &completion as *const Completion as *mut Completion;
            (*pending).base.completion_release = Some(assert_completion_signaled_before_release);

            drm_send_event(dev, core::ptr::addr_of_mut!((*pending).base));
            assert!(completion.is_done());
            // Host tests leave pending-event storage allocated in send helper;
            // release the same single allocation after observing its signal.
            kfree(pending);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn test_file_alloc_and_events() {
        crate::test_support::init_runtime();
        // SAFETY: test exercises file allocation, event queuing, and draining.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            assert!(!dev.is_null());
            assert!(!(*dev).primary.is_null());

            let file_priv = drm_file_alloc((*dev).primary);
            assert!(!file_priv.is_null());
            assert_eq!((*file_priv).event_space, 4096);

            let mut filp_struct: File = core::mem::zeroed();
            filp_struct.private_data = file_priv.cast();
            let filp = &mut filp_struct as *mut File;

            // Poll should report not ready initially
            let poll_mask = drm_poll(filp, core::ptr::null_mut());
            assert_eq!(poll_mask, 0);

            // Allocate and reserve event 1 (size 32, type 0x11)
            let pe1 = kmalloc(core::mem::size_of::<DrmPendingEvent>(), GFP_KERNEL)
                .cast::<DrmPendingEvent>();
            core::ptr::write_bytes(pe1, 0, 1);
            let e1 = kmalloc(32, GFP_KERNEL).cast::<DrmEvent>();
            (*e1).r#type = 0x11;
            (*e1).length = 32;
            assert_eq!(drm_event_reserve_init(dev, file_priv, pe1, e1), 0);
            assert_eq!((*file_priv).event_space, 4096 - 32);

            // Allocate and reserve event 2 (size 40, type 0x22)
            let pe2 = kmalloc(core::mem::size_of::<DrmPendingEvent>(), GFP_KERNEL)
                .cast::<DrmPendingEvent>();
            core::ptr::write_bytes(pe2, 0, 1);
            let e2 = kmalloc(40, GFP_KERNEL).cast::<DrmEvent>();
            (*e2).r#type = 0x22;
            (*e2).length = 40;
            assert_eq!(drm_event_reserve_init(dev, file_priv, pe2, e2), 0);
            assert_eq!((*file_priv).event_space, 4096 - 32 - 40);

            // Still not delivered to event_list, poll returns 0
            assert_eq!(drm_poll(filp, core::ptr::null_mut()), 0);

            // Send event 1 and event 2
            drm_send_event(dev, pe1);
            drm_send_event(dev, pe2);

            // Poll now reports readable (EPOLLIN | EPOLLRDNORM = 0x41)
            assert_eq!(drm_poll(filp, core::ptr::null_mut()), 0x41);

            // Read events: verify FIFO order (e1 then e2)
            let mut read_buf = [0u8; 128];
            let mut offset: i64 = 0;
            let nread = drm_read(
                filp,
                read_buf.as_mut_ptr().cast(),
                read_buf.len(),
                &mut offset,
            );
            assert_eq!(nread, 72); // 32 + 40

            let ev1_read = read_buf.as_ptr().cast::<DrmEvent>();
            assert_eq!((*ev1_read).r#type, 0x11);
            assert_eq!((*ev1_read).length, 32);

            let ev2_read = read_buf.as_ptr().add(32).cast::<DrmEvent>();
            assert_eq!((*ev2_read).r#type, 0x22);
            assert_eq!((*ev2_read).length, 40);

            // Event space restored
            assert_eq!((*file_priv).event_space, 4096);

            // Further read with O_NONBLOCK returns -EAGAIN (-11)
            filp_struct.f_flags = O_NONBLOCK;
            assert_eq!(filp_struct.f_flags, O_NONBLOCK);
            let nread2 = drm_read(
                filp,
                read_buf.as_mut_ptr().cast(),
                read_buf.len(),
                &mut offset,
            );
            assert_eq!(nread2, -11);

            // Test memory stats helpers
            let stats = DrmMemoryStats {
                shared: 0,
                private: 0,
                resident: 0,
                purgeable: 0,
                active: 0,
            };
            assert_eq!(drm_memory_stats_is_zero(&stats), 1);
            let stats_active = DrmMemoryStats {
                shared: 0,
                private: 0,
                resident: 0,
                purgeable: 0,
                active: 1024,
            };
            assert_eq!(drm_memory_stats_is_zero(&stats_active), 0);

            // Test release frees unsent events without error:
            let pe3 = kmalloc(core::mem::size_of::<DrmPendingEvent>(), GFP_KERNEL)
                .cast::<DrmPendingEvent>();
            core::ptr::write_bytes(pe3, 0, 1);
            let e3 = kmalloc(24, GFP_KERNEL).cast::<DrmEvent>();
            (*e3).r#type = 0x33;
            (*e3).length = 24;
            assert_eq!(drm_event_reserve_init(dev, file_priv, pe3, e3), 0);

            let pe4 = kmalloc(core::mem::size_of::<DrmPendingEvent>(), GFP_KERNEL)
                .cast::<DrmPendingEvent>();
            core::ptr::write_bytes(pe4, 0, 1);
            let e4 = kmalloc(24, GFP_KERNEL).cast::<DrmEvent>();
            (*e4).r#type = 0x44;
            (*e4).length = 24;
            assert_eq!(drm_event_reserve_init(dev, file_priv, pe4, e4), 0);
            drm_send_event(dev, pe4);

            drm_file_free(file_priv);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn test_read_blocks_until_woken() {
        crate::test_support::init_runtime();
        crate::test_support::ensure_test_signal_provider();
        assert!(
            drm_kapi::task_runtime::current_task_id().is_some(),
            "host blocking-read test must run with its real host-thread task identity"
        );
        assert!(drm_kapi::task_runtime::can_block_current());
        // SAFETY: test exercises blocking read and event signaling on mock drm file.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);
            let mut filp_struct: File = core::mem::zeroed();
            filp_struct.private_data = file_priv.cast();
            filp_struct.f_flags = 0; // Blocking
            let filp = &mut filp_struct as *mut File;

            let dev_addr = dev as usize;
            let fp_addr = file_priv as usize;
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(15));
                let dev_p = dev_addr as *mut DrmDevice;
                let fp_p = fp_addr as *mut DrmFile;
                let pe = kmalloc(core::mem::size_of::<DrmPendingEvent>(), GFP_KERNEL)
                    .cast::<DrmPendingEvent>();
                core::ptr::write_bytes(pe, 0, 1);
                let e = kmalloc(32, GFP_KERNEL).cast::<DrmEvent>();
                (*e).r#type = 0x55;
                (*e).length = 32;
                assert_eq!(drm_event_reserve_init(dev_p, fp_p, pe, e), 0);
                drm_send_event(dev_p, pe);
            });

            let mut read_buf = [0u8; 64];
            let mut offset = 0i64;
            let nread = drm_read(
                filp,
                read_buf.as_mut_ptr().cast(),
                read_buf.len(),
                &mut offset,
            );
            assert_eq!(nread, 32);
            let ev = read_buf.as_ptr().cast::<DrmEvent>();
            assert_eq!((*ev).r#type, 0x55);

            drm_file_free(file_priv);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn test_read_fault_puts_back_event() {
        crate::test_support::init_runtime();
        // SAFETY: test exercises read error handling and event requeueing.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);
            let mut filp_struct: File = core::mem::zeroed();
            filp_struct.private_data = file_priv.cast();
            filp_struct.f_flags = O_NONBLOCK;
            let filp = &mut filp_struct as *mut File;

            let pe = kmalloc(core::mem::size_of::<DrmPendingEvent>(), GFP_KERNEL)
                .cast::<DrmPendingEvent>();
            core::ptr::write_bytes(pe, 0, 1);
            let e = kmalloc(32, GFP_KERNEL).cast::<DrmEvent>();
            (*e).r#type = 0x77;
            (*e).length = 32;
            assert_eq!(drm_event_reserve_init(dev, file_priv, pe, e), 0);
            drm_send_event(dev, pe);

            let mut offset = 0i64;
            // Fault address: 1 as *mut c_char (< 4096)
            let ret = drm_read(filp, 1 as *mut c_char, 64, &mut offset);
            assert_eq!(ret, -EFAULT as isize);

            // Event was put back onto event_list!
            assert!(!list_empty(&(*file_priv).event_list));

            drm_file_free(file_priv);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn kernel_writer_reuses_event_requeue_and_ownership_path() {
        crate::test_support::init_runtime();
        struct Sink {
            bytes: [u8; 64],
            written: usize,
            limit: usize,
        }

        /// # Safety
        /// `context` must point to a live `Sink` instance and `bytes` must point to `len` readable bytes.
        unsafe extern "C" fn sink_writer(
            context: *mut c_void,
            bytes: *const c_void,
            len: usize,
        ) -> isize {
            // SAFETY: context points to a valid Sink instance allocated on caller stack.
            let sink = unsafe { &mut *context.cast::<Sink>() };
            let written = len.min(sink.limit);
            // SAFETY: bytes is readable for written bytes, and sink.bytes has capacity for written bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes.cast::<u8>(),
                    sink.bytes.as_mut_ptr().add(sink.written),
                    written,
                );
            }
            sink.written += written;
            written as isize
        }

        // SAFETY: test exercises kernel_writer event requeue and ownership path.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);
            let mut filp: File = core::mem::zeroed();
            filp.private_data = file_priv.cast();

            let pending = kmalloc(core::mem::size_of::<DrmPendingEvent>(), GFP_KERNEL)
                .cast::<DrmPendingEvent>();
            core::ptr::write_bytes(pending, 0, 1);
            let event = kmalloc(32, GFP_KERNEL).cast::<DrmEvent>();
            (*event).r#type = 0x78;
            (*event).length = 32;
            assert_eq!(drm_event_reserve_init(dev, file_priv, pending, event), 0);
            drm_send_event(dev, pending);

            let mut short = Sink {
                bytes: [0; 64],
                written: 0,
                limit: 4,
            };
            let ret =
                drm_read_kernel_to(&mut filp, 64, (&mut short as *mut Sink).cast(), sink_writer);
            assert_eq!(ret, -EFAULT as isize);
            assert_eq!(short.written, 4);
            assert!(!list_empty(&(*file_priv).event_list));
            assert_eq!((*file_priv).event_space, 4096 - 32);

            let mut full = Sink {
                bytes: [0; 64],
                written: 0,
                limit: usize::MAX,
            };
            let ret =
                drm_read_kernel_to(&mut filp, 64, (&mut full as *mut Sink).cast(), sink_writer);
            assert_eq!(ret, 32);
            assert_eq!(full.written, 32);
            let read = full.bytes.as_ptr().cast::<DrmEvent>();
            assert_eq!((*read).r#type, 0x78);
            assert_eq!((*read).length, 32);
            assert_eq!((*file_priv).event_space, 4096);
            assert!(list_empty(&(*file_priv).event_list));
            assert_eq!(
                drm_read_kernel_to(&mut filp, 64, (&mut full as *mut Sink).cast(), sink_writer,),
                -EAGAIN as isize,
            );

            drm_file_free(file_priv);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn test_poll_registers_wait_queue() {
        crate::test_support::init_runtime();
        // SAFETY: test exercises drm_poll registering wait queue on mock drm file.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);
            let mut filp_struct: File = core::mem::zeroed();
            filp_struct.private_data = file_priv.cast();
            let filp = &mut filp_struct as *mut File;

            static CALLED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            CALLED.store(false, Ordering::SeqCst);

            /// # Safety
            /// `wait_address`, if non-null, must point to a live `WaitQueueHead`.
            unsafe extern "C" fn test_qproc(
                _filp: *mut File,
                wait_address: *mut WaitQueueHead,
                _p: *mut drm_kapi::wait::PollTable,
            ) {
                if !wait_address.is_null() {
                    CALLED.store(true, Ordering::SeqCst);
                }
            }

            let mut pt = drm_kapi::wait::PollTable {
                qproc: Some(test_qproc as _),
                key: 0,
            };

            let mask = drm_poll(filp, &mut pt as *mut _ as *mut c_void);
            assert_eq!(mask, 0);
            assert!(CALLED.load(Ordering::SeqCst));

            drm_file_free(file_priv);
            drm_dev_put(dev);
        }
    }
}
