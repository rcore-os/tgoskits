// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_ioctl.c`.
//!
//! DRM core ioctl table with flags/permissions, dispatching and copy in/out
//! sizing rules, drm_ioctl_permit, and core ioctl implementations:
//! VERSION, GET_CAP, SET_CLIENT_CAP, GET_UNIQUE, SET_VERSION, GET_CLIENT, GET_STATS.

#![allow(non_snake_case)]

use core::ffi::{c_char, c_uint, c_ulong, c_void};

#[cfg(not(test))]
use drm_kapi::mutex::{mutex_lock, mutex_unlock};
use drm_kapi::{
    config::{CAP_SYS_ADMIN, EFAULT, EINVAL, ENODEV, ENOMEM, EOPNOTSUPP, GFP_KERNEL},
    kernel_services::{capable, current_pid_vnr},
    memory::{kfree, kmalloc},
    mm_native::{copy_from_user, copy_to_user},
};
#[cfg(test)]
/// # Safety
///
/// This test stub performs no memory operations; callers may safely pass any mutex pointer or placeholder.
unsafe fn mutex_lock<M>(_mutex: M) {}
#[cfg(test)]
/// # Safety
///
/// This test stub performs no memory operations; callers may safely pass any mutex pointer or placeholder.
unsafe fn mutex_unlock<M>(_mutex: M) {}
use drm_kapi::types::File;

const IOC_IN: u32 = 1 << 30;
const IOC_OUT: u32 = 2 << 30;
const OVERFLOW_UID: u64 = 65_534;

use crate::{
    drm_auth_upstream::{
        drm_authmagic, drm_dropmaster_ioctl, drm_getmagic, drm_is_current_master,
        drm_setmaster_ioctl,
    },
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_gem_upstream::{drm_gem_close_ioctl, drm_gem_flink_ioctl, drm_gem_open_ioctl},
    drm_ioctl_types::{DRM_AUTH, DRM_MASTER, DRM_RENDER_ALLOW, DRM_ROOT_ONLY, DrmIoctlDesc},
    drm_lease_upstream::{
        drm_mode_create_lease_ioctl, drm_mode_get_lease_ioctl, drm_mode_list_lessees_ioctl,
        drm_mode_revoke_lease_ioctl,
    },
    drm_mode_object_upstream::{DRIVER_ATOMIC, DRIVER_MODESET, drm_core_check_feature},
    drm_prime_upstream::{drm_prime_fd_to_handle_ioctl, drm_prime_handle_to_fd_ioctl},
    uapi_drm_consts::*,
    uapi_drm_types::{
        DrmClient, DrmGetCap, DrmSetClientCap, DrmSetVersion, DrmStats, DrmUnique, DrmVersion,
    },
};

const EACCES: i32 = 13;
pub const DRIVER_SYNCOBJ: u32 = 1 << 5;
pub const DRIVER_SYNCOBJ_TIMELINE: u32 = 1 << 6;

type DrmIoctlFn = unsafe extern "C" fn(*mut DrmDevice, *mut c_void, *mut DrmFile) -> i32;

// upstream: drivers/gpu/drm/drm_ioctl.c drm_noop()
#[unsafe(no_mangle)]
/// # Safety
///
/// This callback ignores all arguments and performs no memory operations.
/// Any pointer values (including null or dangling pointers) may be passed safely.
pub unsafe extern "C" fn drm_noop(
    _dev: *mut DrmDevice,
    _data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    0
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_invalid_op()
#[unsafe(no_mangle)]
/// # Safety
///
/// This callback ignores all arguments and performs no memory operations.
/// Any pointer values (including null or dangling pointers) may be passed safely.
pub unsafe extern "C" fn drm_invalid_op(
    _dev: *mut DrmDevice,
    _data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    -EINVAL
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_copy_field()
/// # Safety
///
/// - If `value` is non-null, it must point to a valid, live, NUL-terminated C string that remains
///   immutable and readable for the duration of the call.
/// - If `buf_len` is non-null, it must be properly aligned and valid for reading and writing a
///   `usize` value, with no concurrent aliases.
/// - If `buf` is non-null, it must point to destination user-space memory of at least `*buf_len`
///   bytes accessible to `copy_to_user`.
unsafe fn drm_copy_field(buf: *mut c_char, buf_len: *mut usize, value: *const c_char) -> i32 {
    if value.is_null() {
        if !buf_len.is_null() {
            // SAFETY: `buf_len` is checked non-null, properly aligned, and valid for writing `usize`
            // with no conflicting borrows per caller contract.
            unsafe {
                *buf_len = 0;
            }
        }
        return 0;
    }
    let mut len = 0;
    // SAFETY:
    // - `value` is verified non-null and caller guarantees it points to a valid NUL-terminated C string,
    //   making pointer additions up to the NUL byte safe.
    // - `buf_len` is null-checked before reading or writing `*buf_len`.
    // - `copy_to_user` writes up to `copy_len` bytes (bounded by the caller-provided `*buf_len`)
    //   from `value` into `buf`, which caller guarantees is accessible user memory.
    unsafe {
        while *value.add(len) != 0 {
            len += 1;
        }
        if !buf_len.is_null() {
            let copy_len = (*buf_len).min(len);
            *buf_len = len;
            if !buf.is_null()
                && copy_len > 0
                && copy_to_user(buf.cast(), value.cast(), copy_len) != 0
            {
                return -EFAULT;
            }
        }
    }
    0
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_version()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `dev` or `data` is null, returns `-EINVAL`.
/// - If non-null, `dev` must point to an initialized, live `DrmDevice`.
/// - If non-null, `data` must be properly aligned for `DrmVersion` and point to writable
///   `DrmVersion` memory. Any user buffer pointers inside (`name`, `date`, `desc`) must either
///   be null or point to user-space memory of at least `name_len`, `date_len`, and `desc_len`
///   bytes accessible via `copy_to_user`.
/// - `_file_priv` is unused and may be any pointer or null.
pub unsafe extern "C" fn drm_version(
    dev: *mut DrmDevice,
    data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    let v = data.cast::<DrmVersion>();
    if dev.is_null() || data.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` and `data` are verified non-null above.
    // - `v` points to properly aligned, writable `DrmVersion` memory as guaranteed by caller contract.
    // - `(*dev).driver` references static or device-lifetime driver metadata (`major`, `minor`, `patchlevel`,
    //   `name`, `desc`).
    // - `drm_copy_field` safely handles copying driver info into the user buffers specified in `*v`.
    unsafe {
        if let Some(driver) = (*dev).driver.as_ref() {
            (*v).version_major = driver.major;
            (*v).version_minor = driver.minor;
            (*v).version_patchlevel = driver.patchlevel;
            let mut name_len = (*v).name_len;
            let mut ret = drm_copy_field((*v).name, &mut name_len, driver.name);
            (*v).name_len = name_len;
            if ret != 0 {
                return ret;
            }
            let mut date_len = (*v).date_len;
            let date_str = c"0".as_ptr().cast();
            ret = drm_copy_field((*v).date, &mut date_len, date_str);
            (*v).date_len = date_len;
            if ret != 0 {
                return ret;
            }
            let mut desc_len = (*v).desc_len;
            ret = drm_copy_field((*v).desc, &mut desc_len, driver.desc);
            (*v).desc_len = desc_len;
            if ret != 0 {
                return ret;
            }
        }
        0
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_getunique()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `dev`, `data`, or `file_priv` is null, returns `-EINVAL`.
/// - If non-null, `dev` must point to an initialized, live `DrmDevice` with an initialized `master_mutex`.
/// - If non-null, `data` must point to an aligned, writable `DrmUnique` struct; if `(*u).unique` is
///   non-null, it must point to user memory of at least `(*u).unique_len` bytes.
/// - If non-null, `file_priv` must point to a live `DrmFile` whose `master` field (if non-null)
///   points to a valid `DrmMaster`.
pub unsafe extern "C" fn drm_getunique(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let u = data.cast::<DrmUnique>();
    if dev.is_null() || data.is_null() || file_priv.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev`, `data`, and `file_priv` are verified non-null above.
    // - Locking `(*dev).master_mutex` serializes concurrent reads and modifications of `(*file_priv).master`.
    // - If `master` is non-null, its `unique` buffer of `unique_len` bytes remains valid while holding `master_mutex`.
    // - `copy_to_user` copies up to `master_len` bytes into the user buffer `(*u).unique`.
    // - `(*u).unique_len` is written safely through the aligned, writable `DrmUnique` pointer `u`.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        let master = (*file_priv).master;
        if master.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return -EINVAL;
        }
        let master_len = (*master).unique_len as usize;
        if (*u).unique_len >= master_len
            && !(*master).unique.is_null()
            && !(*u).unique.is_null()
            && copy_to_user((*u).unique.cast(), (*master).unique.cast(), master_len) != 0
        {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
            return -EFAULT;
        }
        (*u).unique_len = master_len;
        mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
        0
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_getclient()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `data` or `file_priv` is null, returns `-EINVAL`.
/// - If non-null, `data` must point to an aligned, writable `DrmClient` struct.
/// - If non-null, `file_priv` must point to a live `DrmFile` whose `authenticated` field is valid to read.
/// - `_dev` is unused and may be any pointer or null.
pub unsafe extern "C" fn drm_getclient(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let client = data.cast::<DrmClient>();
    if data.is_null() || file_priv.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` and `file_priv` are verified non-null above.
    // - `client` points to properly aligned, writable `DrmClient` memory.
    // - Reading `(*file_priv).authenticated` is safe as `file_priv` is live per caller contract.
    unsafe {
        if (*client).idx == 0 {
            (*client).auth = if (*file_priv).authenticated { 1 } else { 0 };
            // SAFETY: Queries the calling task PID from the active LinuxKPI task context without dereferencing raw pointers.
            let pid = current_pid_vnr();
            (*client).pid = u64::try_from(pid).unwrap_or_default();
            (*client).uid = OVERFLOW_UID;
            (*client).magic = 0;
            (*client).iocs = 0;
            0
        } else {
            -EINVAL
        }
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_getstats()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `data` is null, returns `-EINVAL`.
/// - If non-null, `data` must point to writable memory of at least `size_of::<DrmStats>()` bytes
///   with no conflicting active borrows.
/// - `_dev` and `_file_priv` are unused and may be any pointer or null.
pub unsafe extern "C" fn drm_getstats(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    if data.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is verified non-null above and caller guarantees it points to writable memory
    // of at least `size_of::<DrmStats>()` bytes with no overlapping references.
    unsafe {
        core::ptr::write_bytes(data.cast::<u8>(), 0, core::mem::size_of::<DrmStats>());
        0
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_getcap()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `dev` or `data` is null, returns `-EINVAL`.
/// - If non-null, `dev` must point to an initialized, live `DrmDevice`.
/// - If non-null, `data` must point to an aligned, writable `DrmGetCap` struct whose `capability`
///   field is valid to read.
/// - `_file_priv` is unused and may be any pointer or null.
pub unsafe extern "C" fn drm_getcap(
    dev: *mut DrmDevice,
    data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    let req = data.cast::<DrmGetCap>();
    if dev.is_null() || data.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` and `data` are verified non-null above.
    // - `req` points to an aligned, writable `DrmGetCap` struct.
    // - `drm_core_check_feature` is called with the live, initialized `dev` pointer.
    // - Reading `(*dev).driver` and `(*dev).mode_config` accesses initialized fields on `dev`.
    unsafe {
        (*req).value = 0;
        match (*req).capability as u32 {
            DRM_CAP_TIMESTAMP_MONOTONIC => {
                (*req).value = 1;
                0
            }
            DRM_CAP_PRIME => {
                (*req).value = (DRM_PRIME_CAP_IMPORT | DRM_PRIME_CAP_EXPORT) as u64;
                0
            }
            DRM_CAP_SYNCOBJ => {
                (*req).value = if drm_core_check_feature(dev, DRIVER_SYNCOBJ) {
                    1
                } else {
                    0
                };
                0
            }
            DRM_CAP_SYNCOBJ_TIMELINE => {
                (*req).value = if drm_core_check_feature(dev, DRIVER_SYNCOBJ_TIMELINE) {
                    1
                } else {
                    0
                };
                0
            }
            _ => {
                if !drm_core_check_feature(dev, DRIVER_MODESET) {
                    return -EOPNOTSUPP;
                }
                match (*req).capability as u32 {
                    DRM_CAP_DUMB_BUFFER => {
                        if let Some(driver) = (*dev).driver.as_ref()
                            && driver.dumb_create.is_some()
                        {
                            (*req).value = 1;
                        }
                        0
                    }
                    DRM_CAP_VBLANK_HIGH_CRTC => {
                        (*req).value = 1;
                        0
                    }
                    DRM_CAP_CURSOR_WIDTH => {
                        (*req).value = if (*dev).mode_config.cursor_width != 0 {
                            (*dev).mode_config.cursor_width as u64
                        } else {
                            64
                        };
                        0
                    }
                    DRM_CAP_CURSOR_HEIGHT => {
                        (*req).value = if (*dev).mode_config.cursor_height != 0 {
                            (*dev).mode_config.cursor_height as u64
                        } else {
                            64
                        };
                        0
                    }
                    DRM_CAP_ADDFB2_MODIFIERS => {
                        (*req).value = if (*dev).mode_config.fb_modifiers_not_supported {
                            0
                        } else {
                            1
                        };
                        0
                    }
                    DRM_CAP_CRTC_IN_VBLANK_EVENT => {
                        (*req).value = 1;
                        0
                    }
                    _ => -EINVAL,
                }
            }
        }
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_setclientcap()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `dev`, `data`, or `file_priv` is null, returns `-EINVAL`.
/// - If non-null, `dev` must point to an initialized, live `DrmDevice`.
/// - If non-null, `data` must point to an aligned, readable `DrmSetClientCap` struct.
/// - If non-null, `file_priv` must point to a live `DrmFile` whose client capability fields are
///   valid for mutation without concurrent data races.
pub unsafe extern "C" fn drm_setclientcap(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let req = data.cast::<DrmSetClientCap>();
    if dev.is_null() || data.is_null() || file_priv.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev`, `data`, and `file_priv` were checked non-null above.
    // - `req` points to an aligned `DrmSetClientCap` struct valid for reading `capability` and `value`.
    // - `drm_core_check_feature` is safely invoked on the initialized `dev`.
    // - The calling thread holds exclusive access to `file_priv` during ioctl dispatch, permitting
    //   mutation of `stereo_allowed`, `universal_planes`, `atomic`, `aspect_ratio_allowed`, and `writeback_connectors`.
    unsafe {
        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }
        match (*req).capability as u32 {
            DRM_CLIENT_CAP_STEREO_3D => {
                if (*req).value > 1 {
                    return -EINVAL;
                }
                (*file_priv).stereo_allowed = (*req).value != 0;
            }
            DRM_CLIENT_CAP_UNIVERSAL_PLANES => {
                if (*req).value > 1 {
                    return -EINVAL;
                }
                (*file_priv).universal_planes = (*req).value != 0;
            }
            DRM_CLIENT_CAP_ATOMIC => {
                if !drm_core_check_feature(dev, DRIVER_ATOMIC) {
                    return -EOPNOTSUPP;
                }
                if (*req).value > 2 {
                    return -EINVAL;
                }
                (*file_priv).atomic = (*req).value != 0;
                (*file_priv).universal_planes = (*req).value != 0;
                (*file_priv).aspect_ratio_allowed = (*req).value != 0;
            }
            DRM_CLIENT_CAP_ASPECT_RATIO => {
                if (*req).value > 1 {
                    return -EINVAL;
                }
                (*file_priv).aspect_ratio_allowed = (*req).value != 0;
            }
            DRM_CLIENT_CAP_WRITEBACK_CONNECTORS => {
                if !(*file_priv).atomic {
                    return -EINVAL;
                }
                if (*req).value > 1 {
                    return -EINVAL;
                }
                (*file_priv).writeback_connectors = (*req).value != 0;
            }
            _ => return -EINVAL,
        }
        0
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_setversion()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `dev` or `data` is null, returns `-EINVAL`.
/// - If non-null, `dev` must point to an initialized, live `DrmDevice` containing `master_mutex`.
/// - If non-null, `data` must point to an aligned, readable and writable `DrmSetVersion` struct.
/// - `_file_priv` is unused and may be any pointer or null.
pub unsafe extern "C" fn drm_setversion(
    dev: *mut DrmDevice,
    data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    let sv = data.cast::<DrmSetVersion>();
    if dev.is_null() || data.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` and `data` were checked non-null above.
    // - `sv` points to an aligned `DrmSetVersion` struct valid for reading and writing version fields.
    // - `mutex_lock` / `mutex_unlock` operate on the initialized `(*dev).master_mutex`.
    // - `(*dev).driver` is read while holding `master_mutex`, accessing static driver fields.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).master_mutex));
        let mut ret = 0;
        if (*sv).drm_di_major != -1
            && ((*sv).drm_di_major != 1 || (*sv).drm_di_minor < 0 || (*sv).drm_di_minor > 4)
        {
            ret = -EINVAL;
        }
        (*sv).drm_di_major = 1;
        (*sv).drm_di_minor = 4;
        if let Some(driver) = (*dev).driver.as_ref() {
            (*sv).drm_dd_major = driver.major;
            (*sv).drm_dd_minor = driver.minor;
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).master_mutex));
        ret
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_set_client_name()
#[unsafe(no_mangle)]
/// # Safety
///
/// This callback ignores all arguments and performs no memory operations.
/// Any pointer values (including null or dangling pointers) may be passed safely.
pub unsafe extern "C" fn drm_set_client_name(
    _dev: *mut DrmDevice,
    _data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    0
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_ioctl_permit()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `file_priv` is null, returns `-EINVAL`.
/// - If non-null, `file_priv` must point to an initialized, live `DrmFile` with a valid, non-null
///   `minor` pointer to a `DrmMinor`.
pub unsafe extern "C" fn drm_ioctl_permit(flags: u32, file_priv: *mut DrmFile) -> i32 {
    if file_priv.is_null() {
        return -EINVAL;
    }
    // SAFETY: `file_priv` is verified non-null above.
    // - `(*file_priv).minor` is guaranteed non-null and valid for an open DRM file, allowing
    //   reading `(*(*file_priv).minor).r#type` to check for render nodes.
    // - `(*file_priv).authenticated` is safely read from the live `file_priv`.
    // - `drm_is_current_master` queries the master state of `file_priv`.
    unsafe {
        if (flags & DRM_ROOT_ONLY != 0) && !capable(CAP_SYS_ADMIN) {
            return -EACCES;
        }
        let is_render = (*(*file_priv).minor).r#type == 2;
        if (flags & DRM_AUTH != 0) && !is_render && !(*file_priv).authenticated {
            return -EACCES;
        }
        if (flags & DRM_MASTER != 0) && !drm_is_current_master(file_priv) {
            return -EACCES;
        }
        if (flags & DRM_RENDER_ALLOW == 0) && is_render {
            return -EACCES;
        }
        0
    }
}

// Core ioctl dispatcher lookup
macro_rules! ioctl_entry {
    ($cmd:ident, $func:ident, $flags:expr) => {
        DrmIoctlDesc {
            cmd: $cmd,
            flags: $flags,
            func: $func as *mut c_void,
            name: core::ptr::null_mut(),
        }
    };
}

unsafe extern "C" {
    fn drm_wait_vblank_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_getresources(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_getplane_res(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_getcrtc(dev: *mut DrmDevice, data: *mut c_void, file_priv: *mut DrmFile) -> i32;
    fn drm_mode_setcrtc(dev: *mut DrmDevice, data: *mut c_void, file_priv: *mut DrmFile) -> i32;
    fn drm_mode_getplane(dev: *mut DrmDevice, data: *mut c_void, file_priv: *mut DrmFile) -> i32;
    fn drm_mode_setplane(dev: *mut DrmDevice, data: *mut c_void, file_priv: *mut DrmFile) -> i32;
    fn drm_mode_cursor_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_cursor2_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_gamma_get_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_gamma_set_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_getencoder(dev: *mut DrmDevice, data: *mut c_void, file_priv: *mut DrmFile) -> i32;
    fn drm_mode_getconnector(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_getproperty_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_connector_property_set_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_getblob_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_createblob_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_destroyblob_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_getfb(dev: *mut DrmDevice, data: *mut c_void, file_priv: *mut DrmFile) -> i32;
    fn drm_mode_getfb2_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_addfb_ioctl(dev: *mut DrmDevice, data: *mut c_void, file_priv: *mut DrmFile)
    -> i32;
    fn drm_mode_addfb2_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_rmfb_ioctl(dev: *mut DrmDevice, data: *mut c_void, file_priv: *mut DrmFile) -> i32;
    fn drm_mode_closefb_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_page_flip_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_dirtyfb_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_create_dumb_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_mmap_dumb_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_destroy_dumb_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_obj_get_properties_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_obj_set_property_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_mode_atomic_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_create_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_destroy_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_handle_to_fd_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_fd_to_handle_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_transfer_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_wait_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_timeline_wait_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_eventfd_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_reset_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_signal_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_timeline_signal_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_syncobj_query_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_crtc_get_sequence_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
    fn drm_crtc_queue_sequence_ioctl(
        dev: *mut DrmDevice,
        data: *mut c_void,
        file_priv: *mut DrmFile,
    ) -> i32;
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_ioctls
pub static DRM_IOCTLS: &[DrmIoctlDesc] = &[
    ioctl_entry!(DRM_IOCTL_VERSION, drm_version, DRM_RENDER_ALLOW),
    ioctl_entry!(DRM_IOCTL_GET_UNIQUE, drm_getunique, 0),
    ioctl_entry!(DRM_IOCTL_GET_MAGIC, drm_getmagic, 0),
    ioctl_entry!(DRM_IOCTL_GET_CLIENT, drm_getclient, 0),
    ioctl_entry!(DRM_IOCTL_GET_STATS, drm_getstats, 0),
    ioctl_entry!(DRM_IOCTL_GET_CAP, drm_getcap, DRM_RENDER_ALLOW),
    ioctl_entry!(DRM_IOCTL_SET_CLIENT_CAP, drm_setclientcap, 0),
    ioctl_entry!(DRM_IOCTL_SET_VERSION, drm_setversion, DRM_MASTER),
    ioctl_entry!(
        DRM_IOCTL_SET_UNIQUE,
        drm_invalid_op,
        DRM_AUTH | DRM_MASTER | DRM_ROOT_ONLY
    ),
    ioctl_entry!(
        DRM_IOCTL_BLOCK,
        drm_noop,
        DRM_AUTH | DRM_MASTER | DRM_ROOT_ONLY
    ),
    ioctl_entry!(
        DRM_IOCTL_UNBLOCK,
        drm_noop,
        DRM_AUTH | DRM_MASTER | DRM_ROOT_ONLY
    ),
    ioctl_entry!(DRM_IOCTL_AUTH_MAGIC, drm_authmagic, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_SET_MASTER, drm_setmaster_ioctl, 0),
    ioctl_entry!(DRM_IOCTL_DROP_MASTER, drm_dropmaster_ioctl, 0),
    ioctl_entry!(
        DRM_IOCTL_ADD_DRAW,
        drm_noop,
        DRM_AUTH | DRM_MASTER | DRM_ROOT_ONLY
    ),
    ioctl_entry!(
        DRM_IOCTL_RM_DRAW,
        drm_noop,
        DRM_AUTH | DRM_MASTER | DRM_ROOT_ONLY
    ),
    ioctl_entry!(DRM_IOCTL_FINISH, drm_noop, DRM_AUTH),
    ioctl_entry!(DRM_IOCTL_WAIT_VBLANK, drm_wait_vblank_ioctl, 0),
    ioctl_entry!(
        DRM_IOCTL_UPDATE_DRAW,
        drm_noop,
        DRM_AUTH | DRM_MASTER | DRM_ROOT_ONLY
    ),
    ioctl_entry!(DRM_IOCTL_GEM_CLOSE, drm_gem_close_ioctl, DRM_RENDER_ALLOW),
    ioctl_entry!(DRM_IOCTL_GEM_FLINK, drm_gem_flink_ioctl, DRM_AUTH),
    ioctl_entry!(DRM_IOCTL_GEM_OPEN, drm_gem_open_ioctl, DRM_AUTH),
    ioctl_entry!(
        DRM_IOCTL_GEM_CHANGE_HANDLE,
        drm_invalid_op,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(DRM_IOCTL_MODE_GETRESOURCES, drm_mode_getresources, 0),
    ioctl_entry!(
        DRM_IOCTL_PRIME_HANDLE_TO_FD,
        drm_prime_handle_to_fd_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_PRIME_FD_TO_HANDLE,
        drm_prime_fd_to_handle_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SET_CLIENT_NAME,
        drm_set_client_name,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(DRM_IOCTL_MODE_GETPLANERESOURCES, drm_mode_getplane_res, 0),
    ioctl_entry!(DRM_IOCTL_MODE_GETCRTC, drm_mode_getcrtc, 0),
    ioctl_entry!(DRM_IOCTL_MODE_SETCRTC, drm_mode_setcrtc, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_MODE_GETPLANE, drm_mode_getplane, 0),
    ioctl_entry!(DRM_IOCTL_MODE_SETPLANE, drm_mode_setplane, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_MODE_CURSOR, drm_mode_cursor_ioctl, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_MODE_GETGAMMA, drm_mode_gamma_get_ioctl, 0),
    ioctl_entry!(
        DRM_IOCTL_MODE_SETGAMMA,
        drm_mode_gamma_set_ioctl,
        DRM_MASTER
    ),
    ioctl_entry!(DRM_IOCTL_MODE_GETENCODER, drm_mode_getencoder, 0),
    ioctl_entry!(DRM_IOCTL_MODE_GETCONNECTOR, drm_mode_getconnector, 0),
    ioctl_entry!(DRM_IOCTL_MODE_ATTACHMODE, drm_noop, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_MODE_DETACHMODE, drm_noop, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_MODE_GETPROPERTY, drm_mode_getproperty_ioctl, 0),
    ioctl_entry!(
        DRM_IOCTL_MODE_SETPROPERTY,
        drm_connector_property_set_ioctl,
        DRM_MASTER
    ),
    ioctl_entry!(DRM_IOCTL_MODE_GETPROPBLOB, drm_mode_getblob_ioctl, 0),
    ioctl_entry!(DRM_IOCTL_MODE_GETFB, drm_mode_getfb, 0),
    ioctl_entry!(DRM_IOCTL_MODE_GETFB2, drm_mode_getfb2_ioctl, 0),
    ioctl_entry!(DRM_IOCTL_MODE_ADDFB, drm_mode_addfb_ioctl, 0),
    ioctl_entry!(DRM_IOCTL_MODE_ADDFB2, drm_mode_addfb2_ioctl, 0),
    ioctl_entry!(DRM_IOCTL_MODE_RMFB, drm_mode_rmfb_ioctl, 0),
    ioctl_entry!(DRM_IOCTL_MODE_CLOSEFB, drm_mode_closefb_ioctl, 0),
    ioctl_entry!(
        DRM_IOCTL_MODE_PAGE_FLIP,
        drm_mode_page_flip_ioctl,
        DRM_MASTER
    ),
    ioctl_entry!(DRM_IOCTL_MODE_DIRTYFB, drm_mode_dirtyfb_ioctl, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_MODE_CREATE_DUMB, drm_mode_create_dumb_ioctl, 0),
    ioctl_entry!(DRM_IOCTL_MODE_MAP_DUMB, drm_mode_mmap_dumb_ioctl, 0),
    ioctl_entry!(DRM_IOCTL_MODE_DESTROY_DUMB, drm_mode_destroy_dumb_ioctl, 0),
    ioctl_entry!(
        DRM_IOCTL_MODE_OBJ_GETPROPERTIES,
        drm_mode_obj_get_properties_ioctl,
        0
    ),
    ioctl_entry!(
        DRM_IOCTL_MODE_OBJ_SETPROPERTY,
        drm_mode_obj_set_property_ioctl,
        DRM_MASTER
    ),
    ioctl_entry!(DRM_IOCTL_MODE_CURSOR2, drm_mode_cursor2_ioctl, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_MODE_ATOMIC, drm_mode_atomic_ioctl, DRM_MASTER),
    ioctl_entry!(DRM_IOCTL_MODE_CREATEPROPBLOB, drm_mode_createblob_ioctl, 0),
    ioctl_entry!(
        DRM_IOCTL_MODE_DESTROYPROPBLOB,
        drm_mode_destroyblob_ioctl,
        0
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_CREATE,
        drm_syncobj_create_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_DESTROY,
        drm_syncobj_destroy_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD,
        drm_syncobj_handle_to_fd_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE,
        drm_syncobj_fd_to_handle_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_TRANSFER,
        drm_syncobj_transfer_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_WAIT,
        drm_syncobj_wait_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT,
        drm_syncobj_timeline_wait_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_EVENTFD,
        drm_syncobj_eventfd_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_RESET,
        drm_syncobj_reset_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_SIGNAL,
        drm_syncobj_signal_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL,
        drm_syncobj_timeline_signal_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(
        DRM_IOCTL_SYNCOBJ_QUERY,
        drm_syncobj_query_ioctl,
        DRM_RENDER_ALLOW
    ),
    ioctl_entry!(DRM_IOCTL_CRTC_GET_SEQUENCE, drm_crtc_get_sequence_ioctl, 0),
    ioctl_entry!(
        DRM_IOCTL_CRTC_QUEUE_SEQUENCE,
        drm_crtc_queue_sequence_ioctl,
        0
    ),
    ioctl_entry!(
        DRM_IOCTL_MODE_CREATE_LEASE,
        drm_mode_create_lease_ioctl,
        DRM_MASTER
    ),
    ioctl_entry!(
        DRM_IOCTL_MODE_LIST_LESSEES,
        drm_mode_list_lessees_ioctl,
        DRM_MASTER
    ),
    ioctl_entry!(
        DRM_IOCTL_MODE_GET_LEASE,
        drm_mode_get_lease_ioctl,
        DRM_MASTER
    ),
    ioctl_entry!(
        DRM_IOCTL_MODE_REVOKE_LEASE,
        drm_mode_revoke_lease_ioctl,
        DRM_MASTER
    ),
];

// upstream: drivers/gpu/drm/drm_ioctl.c drm_ioctl_kernel()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `filp` is null or `(*filp).private_data` is null, returns `-EINVAL`.
/// - If non-null, `filp` must point to an open, live `File` whose `private_data` points to an
///   initialized `DrmFile` with valid `minor` and `dev` backpointers.
/// - `func` must be an ioctl dispatch function conforming to `DrmIoctlFn`.
/// - `kdata` must point to a kernel buffer sufficiently sized and aligned for the requirements
///   of `func`.
pub unsafe extern "C" fn drm_ioctl_kernel(
    filp: *mut File,
    func: DrmIoctlFn,
    kdata: *mut c_void,
    flags: u32,
) -> i32 {
    if filp.is_null() {
        return -EINVAL;
    }
    // SAFETY: `filp` is verified non-null above.
    // - `(*filp).private_data` is null-checked; if non-null, it points to a live `DrmFile`.
    // - `(*(*file_priv).minor).dev` is assumed valid for an initialized DRM open file.
    // - `(*dev).unplugged` is an atomic load with Acquire ordering.
    // - `drm_ioctl_permit` performs permission checks on `file_priv`.
    // - `func` is a function pointer adhering to `DrmIoctlFn`, invoked with valid `dev`, `kdata`, and `file_priv`.
    unsafe {
        let file_priv = (*filp).private_data.cast::<DrmFile>();
        if file_priv.is_null() {
            return -EINVAL;
        }
        let dev = (*(*file_priv).minor).dev;
        if (*dev).unplugged.load(core::sync::atomic::Ordering::Acquire) {
            return -ENODEV;
        }
        let ret = drm_ioctl_permit(flags, file_priv);
        if ret != 0 {
            return ret;
        }
        func(dev, kdata, file_priv)
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `filp` is null, returns `-EINVAL`.
/// - If non-null, `filp` must point to an open, live `File` whose `private_data` points to an
///   initialized `DrmFile` with valid `minor` and `dev` pointers.
/// - If the ioctl command transfers data (`IOC_IN` / `IOC_OUT`), `arg` must be a valid user-space
///   virtual address accessible to `copy_from_user` and `copy_to_user` for the payload length.
pub unsafe extern "C" fn drm_ioctl(filp: *mut File, cmd: c_uint, arg: c_ulong) -> i32 {
    if filp.is_null() {
        return -EINVAL;
    }
    // SAFETY: `filp` is checked non-null above.
    // - `(*filp).private_data` is cast and null-checked, yielding a live `DrmFile`.
    // - `(*(*file_priv).minor).dev` is valid for an open DRM file; `(*dev).unplugged` is loaded atomically.
    // - `desc` is resolved from either static table `DRM_IOCTLS` or driver-specific table `(*dev).driver.ioctls`.
    // - `(*desc).func` is checked non-null before transmute to `DrmIoctlFn`.
    // - `kdata` points either to 16-byte aligned local stack buffer `stack` or to memory allocated via `kmalloc`.
    // - `copy_from_user` and `copy_to_user` safely perform user-space memory access for `arg`.
    // - `core::ptr::write_bytes` zeroes trailing uncopied kernel buffer space up to `ksize`.
    // - If dynamically allocated via `kmalloc`, `kdata` is freed via `kfree` on all exit paths.
    unsafe {
        let file_priv = (*filp).private_data.cast::<DrmFile>();
        if file_priv.is_null() {
            return -EINVAL;
        }
        let dev = (*(*file_priv).minor).dev;
        if (*dev).unplugged.load(core::sync::atomic::Ordering::Acquire) {
            return -ENODEV;
        }

        let nr = cmd & 0xff;
        let is_driver = (0x40..0xa0).contains(&nr);

        let desc: *const DrmIoctlDesc = if is_driver {
            let idx = (nr - 0x40) as usize;
            if let Some(driver) = (*dev).driver.as_ref() {
                if idx < driver.num_ioctls as usize && !driver.ioctls.is_null() {
                    driver.ioctls.add(idx)
                } else {
                    return -EINVAL;
                }
            } else {
                return -EINVAL;
            }
        } else {
            let mut found: *const DrmIoctlDesc = core::ptr::null();
            for item in DRM_IOCTLS.iter() {
                if (item.cmd & 0xff) == nr {
                    found = item;
                    break;
                }
            }
            if found.is_null() {
                return -EINVAL;
            }
            found
        };

        // upstream: drm_ioctl(): `if (unlikely(!func))` -> -EINVAL before any copy.
        // Driver tables may contain zeroed gaps (designated-initializer holes).
        if (*desc).func.is_null() {
            return -EINVAL;
        }
        let func: DrmIoctlFn = core::mem::transmute((*desc).func);
        let drv_size = ((*desc).cmd >> 16) & 0x3fff;
        let mut in_size = (cmd >> 16) & 0x3fff;
        let mut out_size = in_size;
        if ((*desc).cmd & (IOC_IN | IOC_OUT)) != 0 {
            if (cmd & (*desc).cmd & IOC_IN) == 0 {
                in_size = 0;
            }
            if (cmd & (*desc).cmd & IOC_OUT) == 0 {
                out_size = 0;
            }
        }
        let ksize = drv_size.max(in_size).max(out_size) as usize;

        // Handlers reinterpret the buffer as their UAPI struct, so give it the
        // 16-byte alignment the C ABI gives Linux's `char stack_kdata[128]`.
        #[repr(C, align(16))]
        struct StackKdata([u8; 128]);
        let mut stack = StackKdata([0; 128]);
        let stack_ptr = stack.0.as_mut_ptr();
        let kdata = if ksize <= stack.0.len() {
            stack_ptr
        } else {
            kmalloc(ksize, GFP_KERNEL).cast::<u8>()
        };
        if kdata.is_null() {
            return -ENOMEM;
        }

        if in_size > 0 && copy_from_user(kdata.cast(), arg as *const c_void, in_size as usize) != 0
        {
            if kdata != stack_ptr {
                kfree(kdata.cast::<c_void>());
            }
            return -EFAULT;
        }
        if ksize > (in_size as usize) {
            core::ptr::write_bytes(kdata.add(in_size as usize), 0, ksize - in_size as usize);
        }

        let mut ret = drm_ioctl_kernel(filp, func, kdata.cast(), (*desc).flags);

        if out_size > 0 && copy_to_user(arg as *mut c_void, kdata.cast(), out_size as usize) != 0 {
            ret = -EFAULT;
        }

        if kdata != stack_ptr {
            kfree(kdata.cast::<c_void>());
        }

        ret
    }
}

// upstream: drivers/gpu/drm/drm_ioctl.c drm_ioctl_flags()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `flags` is non-null, it must be properly aligned and valid for writing a `c_uint` without
/// concurrent conflicting borrows. If `flags` is null, no write is performed.
pub unsafe extern "C" fn drm_ioctl_flags(nr: c_uint, flags: *mut c_uint) -> bool {
    if nr >= DRM_COMMAND_BASE as c_uint && nr < DRM_COMMAND_END as c_uint {
        return false;
    }
    if nr as usize >= DRM_IOCTLS.len() {
        return false;
    }
    if !flags.is_null() {
        // SAFETY: `flags` is non-null, properly aligned, and writable for `c_uint` as required by the caller contract.
        // `nr` is bounds-checked against `DRM_IOCTLS.len()` above.
        unsafe { *flags = DRM_IOCTLS[nr as usize].flags };
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        drm_drv_types::DrmDriver,
        drm_drv_upstream::{drm_dev_alloc, drm_dev_put},
        drm_file_upstream::{drm_file_alloc, drm_file_free},
        uapi_drm_consts::DRM_IOCTL_VERSION,
        uapi_drm_types::DrmVersion,
    };

    #[test]
    fn test_ioctl_permit_flags() {
        crate::test_support::init_runtime();
        // SAFETY: Test allocates mock DrmDevice and DrmFile structures to test ioctl permit logic, then cleans them up.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            assert!(!dev.is_null());

            let primary_file = drm_file_alloc((*dev).primary);
            assert!(!primary_file.is_null());
            assert_eq!(crate::drm_auth_upstream::drm_master_open(primary_file), 0);

            let render_file = drm_file_alloc((*dev).render);
            assert!(!render_file.is_null());

            // 1. DRM_ROOT_ONLY: without CAP_SYS_ADMIN provider, returns -EACCES
            assert_eq!(drm_ioctl_permit(DRM_ROOT_ONLY, primary_file), -EACCES);

            // 2. DRM_AUTH on primary file:
            (*primary_file).authenticated = false;
            assert_eq!(drm_ioctl_permit(DRM_AUTH, primary_file), -EACCES);
            (*primary_file).authenticated = true;
            assert_eq!(drm_ioctl_permit(DRM_AUTH, primary_file), 0);

            // 3. DRM_MASTER on primary file:
            // Since primary_file was first opened, it is master
            assert_eq!(drm_ioctl_permit(DRM_MASTER, primary_file), 0);
            (*primary_file).is_master = false;
            assert_eq!(drm_ioctl_permit(DRM_MASTER, primary_file), -EACCES);

            // 4. Render node permissions:
            // Without DRM_RENDER_ALLOW -> -EACCES
            assert_eq!(drm_ioctl_permit(0, render_file), -EACCES);
            // With DRM_RENDER_ALLOW -> 0
            assert_eq!(drm_ioctl_permit(DRM_RENDER_ALLOW, render_file), 0);
            // With DRM_RENDER_ALLOW | DRM_AUTH -> bypasses AUTH check on render node
            (*render_file).authenticated = false;
            assert_eq!(
                drm_ioctl_permit(DRM_RENDER_ALLOW | DRM_AUTH, render_file),
                0
            );

            drm_file_free(render_file);
            drm_file_free(primary_file);
            drm_dev_put(dev);
        }
    }

    #[repr(C)]
    struct DriverTestData {
        a: u64,
        b: u64,
    }

    use core::sync::atomic::{AtomicU64, Ordering};
    static RECEIVED_A: AtomicU64 = AtomicU64::new(0);
    static RECEIVED_B: AtomicU64 = AtomicU64::new(0);

    /// # Safety
    /// `data` must point to a valid, aligned `DriverTestData` buffer.
    unsafe extern "C" fn test_driver_ioctl(
        _dev: *mut DrmDevice,
        data: *mut c_void,
        _file_priv: *mut DrmFile,
    ) -> i32 {
        // SAFETY: data is guaranteed by the test caller to point to a valid, aligned DriverTestData struct.
        unsafe {
            let d = data.cast::<DriverTestData>();
            RECEIVED_A.store((*d).a, Ordering::SeqCst);
            RECEIVED_B.store((*d).b, Ordering::SeqCst);
            (*d).a = 0x1111;
            (*d).b = 0x2222;
            0
        }
    }

    #[test]
    fn test_ioctl_dispatch_sizing_and_unknown() {
        crate::test_support::init_runtime();
        // SAFETY: Test allocates mock driver, device, and file structures to test ioctl dispatch sizing rules, then cleans them up.
        unsafe {
            let mut driver: DrmDriver = core::mem::zeroed();
            driver.major = 1;
            driver.minor = 2;
            driver.patchlevel = 3;
            driver.name = c"testdrv".as_ptr().cast_mut().cast();
            driver.desc = c"Test Driver".as_ptr().cast_mut().cast();

            // Setup custom driver ioctl at nr 0x40 (DRM_COMMAND_BASE)
            let drv_desc = DrmIoctlDesc {
                cmd: (size_of::<DriverTestData>() as u32) << 16 | 0x40,
                flags: DRM_RENDER_ALLOW,
                func: test_driver_ioctl as *mut c_void,
                name: core::ptr::null_mut(),
            };
            driver.ioctls = (&drv_desc as *const DrmIoctlDesc).cast_mut();
            driver.num_ioctls = 1;

            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);

            let mut filp_struct: File = core::mem::zeroed();
            filp_struct.private_data = file_priv.cast();
            let filp = &mut filp_struct as *mut File;

            // 1. Unknown ioctl: nr 0xff (unknown core ioctl) -> -EINVAL
            assert_eq!(drm_ioctl(filp, 0x0000_00ff, 0), -EINVAL);

            // 2. Standard core ioctl: DRM_IOCTL_VERSION
            let mut version_data: DrmVersion = core::mem::zeroed();
            let ret = drm_ioctl(
                filp,
                DRM_IOCTL_VERSION,
                &mut version_data as *mut DrmVersion as usize as u64,
            );
            assert_eq!(ret, 0);
            assert_eq!(version_data.version_major, 1);
            assert_eq!(version_data.version_minor, 2);
            assert_eq!(version_data.version_patchlevel, 3);

            // 3. Zero-extension rule: user passes 8 bytes (user_size = 8 < drv_size = 16)
            #[repr(C, align(8))]
            struct AlignedBuf<const N: usize>([u8; N]);

            let mut user_buf_8 = AlignedBuf([0xaa; 8]);
            let cmd_user_8 = (8u32 << 16) | 0x40;
            RECEIVED_A.store(0, Ordering::SeqCst);
            RECEIVED_B.store(0, Ordering::SeqCst);
            let ret = drm_ioctl(filp, cmd_user_8, user_buf_8.0.as_mut_ptr() as usize as u64);
            assert_eq!(ret, 0);
            assert_eq!(RECEIVED_A.load(Ordering::SeqCst), 0xaaaaaaaaaaaaaaaa);
            assert_eq!(RECEIVED_B.load(Ordering::SeqCst), 0); // zero-extended!

            // 4. Truncation rule: user passes 24 bytes (user_size = 24 > drv_size = 16)
            let mut user_buf_24 = AlignedBuf([0u8; 24]);
            let d_in = user_buf_24.0.as_mut_ptr().cast::<DriverTestData>();
            (*d_in).a = 0x5555;
            (*d_in).b = 0x6666;
            let cmd_user_24 = (24u32 << 16) | 0x40;
            let ret = drm_ioctl(
                filp,
                cmd_user_24,
                user_buf_24.0.as_mut_ptr() as usize as u64,
            );
            assert_eq!(ret, 0);
            assert_eq!(RECEIVED_A.load(Ordering::SeqCst), 0x5555);
            assert_eq!(RECEIVED_B.load(Ordering::SeqCst), 0x6666);
            // Copied back 16 bytes:
            assert_eq!((*d_in).a, 0x1111);
            assert_eq!((*d_in).b, 0x2222);
            // Trailing 8 bytes untouched:
            assert_eq!(user_buf_24.0[16], 0);

            // 5. Fault rule: invalid user pointer (< 4096) returns -EFAULT
            let ret_fault = drm_ioctl(filp, DRM_IOCTL_VERSION, 0x10);
            assert_eq!(ret_fault, -EFAULT);

            drm_file_free(file_priv);
            drm_dev_put(dev);
        }
    }
}
