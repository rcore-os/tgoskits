// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_dumb_buffers.c` helpers used by DRM clients.

use core::ffi::c_void;

use drm_kapi::config::{EINVAL, PAGE_SIZE};

use crate::{
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_fourcc_types::DrmFormatInfo,
    uapi_drm_consts::DRM_FORMAT_INVALID,
    uapi_drm_mode_types::{DrmModeCreateDumb, DrmModeDestroyDumb, DrmModeMapDumb},
};

const ENOSYS: i32 = 38;

// upstream: drm_dumb_buffers.c drm_mode_align_dumb()
/// # Safety
///
/// `args` must point to a live, properly aligned, and mutable `DrmModeCreateDumb`
/// structure that is exclusively accessible for the duration of this call.
unsafe fn drm_mode_align_dumb(
    args: *mut DrmModeCreateDumb,
    hw_pitch_align: usize,
    hw_size_align: usize,
) -> i32 {
    // SAFETY: Per function safety contract, `args` is non-null, correctly aligned,
    // and points to an exclusively accessed `DrmModeCreateDumb` struct.
    unsafe {
        let mut pitch = (*args).pitch as usize;
        if pitch == 0 {
            return -EINVAL;
        }
        if hw_pitch_align != 0 {
            let Some(rounded) = pitch.checked_add(hw_pitch_align - 1) else {
                return -EINVAL;
            };
            pitch = rounded / hw_pitch_align * hw_pitch_align;
            if pitch < hw_pitch_align || pitch > u32::MAX as usize {
                return -EINVAL;
            }
        }
        let size_align = if hw_size_align == 0 {
            PAGE_SIZE
        } else if hw_size_align.is_multiple_of(PAGE_SIZE) {
            hw_size_align
        } else {
            return -EINVAL;
        };
        let Some(size) = ((*args).height as usize).checked_mul(pitch) else {
            return -EINVAL;
        };
        let Some(rounded) = size.checked_add(size_align - 1) else {
            return -EINVAL;
        };
        let size = rounded / size_align * size_align;
        if size == 0 || size > u32::MAX as usize {
            return -EINVAL;
        }
        (*args).pitch = pitch as u32;
        (*args).size = size as u64;
        0
    }
}

// upstream: drm_dumb_buffers.c drm_mode_size_dumb()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `dev` must point to a live, initialized `DrmDevice`, and `args` must point
/// to an aligned, mutable `DrmModeCreateDumb` structure that is exclusively accessible.
pub unsafe extern "C" fn drm_mode_size_dumb(
    dev: *mut DrmDevice,
    args: *mut DrmModeCreateDumb,
    hw_pitch_align: usize,
    hw_size_align: usize,
) -> i32 {
    if dev.is_null() || args.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` and `args` are verified non-null above, and caller contract guarantees
    // they point to valid, properly aligned, live structures. `args` is exclusively accessed.
    unsafe {
        let format = crate::drm_fourcc_upstream::drm_driver_color_mode_format(dev, (*args).bpp);
        let pitch = if format != DRM_FORMAT_INVALID {
            let info: *const DrmFormatInfo = crate::drm_fourcc_upstream::drm_format_info(format);
            if info.is_null() {
                return -EINVAL;
            }
            crate::drm_fourcc_upstream::drm_format_info_min_pitch(info, 0, (*args).width)
        } else if (*args).bpp != 0 {
            let bytes = (*args).bpp.div_ceil(8);
            (*args).width as u64 * bytes as u64
        } else {
            0
        };
        if pitch == 0 || pitch > u32::MAX as u64 {
            return -EINVAL;
        }
        (*args).pitch = pitch as u32;
        drm_mode_align_dumb(args, hw_pitch_align, hw_size_align)
    }
}

// upstream: drm_dumb_buffers.c drm_mode_create_dumb()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev`, `args`, and `file` must point to live, initialized DRM structures.
/// `args` must point to an aligned, mutable `DrmModeCreateDumb` structure exclusively accessed.
pub unsafe extern "C" fn drm_mode_create_dumb(
    dev: *mut DrmDevice,
    args: *mut DrmModeCreateDumb,
    file: *mut DrmFile,
) -> i32 {
    if dev.is_null() || args.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev`, `args`, and `file` are non-null and valid per caller contract.
    // The driver pointer and dumb_create callback are checked before invocation.
    unsafe {
        let driver = (*dev).driver;
        if driver.is_null() || (*driver).dumb_create.is_none() {
            return -ENOSYS;
        }
        if (*args).width == 0
            || (*args).height == 0
            || (*args).bpp == 0
            || (*args).bpp > u32::MAX - 8
        {
            return -EINVAL;
        }
        let cpp = (*args).bpp.div_ceil(8);
        let Some(stride) = cpp.checked_mul((*args).width) else {
            return -EINVAL;
        };
        let Some(size) = stride.checked_mul((*args).height) else {
            return -EINVAL;
        };
        if size.checked_add(PAGE_SIZE as u32 - 1).is_none() {
            return -EINVAL;
        }
        (*args).handle = 0;
        (*args).pitch = 0;
        (*args).size = 0;
        ((*driver).dumb_create.unwrap())(file, dev, args)
    }
}

// upstream: drm_dumb_buffers.c drm_mode_create_dumb_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` and `file` must point to live DRM structures. If non-null, `data` must point to
/// a properly aligned, mutable `DrmModeCreateDumb` structure exclusively accessible.
pub unsafe extern "C" fn drm_mode_create_dumb_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if data.is_null() {
        return -EINVAL;
    }
    let args = data.cast::<DrmModeCreateDumb>();
    // SAFETY: `data` is non-null, aligned, and valid for `DrmModeCreateDumb` per caller contract.
    // `dev` and `file` are forwarded as validated pointers.
    let ret = unsafe { drm_mode_create_dumb(dev, args, file) };
    if ret != 0 {
        // SAFETY: On failure, `args` remains valid, non-null, and exclusively accessible to clear output fields.
        unsafe {
            (*args).handle = 0;
            (*args).pitch = 0;
            (*args).size = 0;
        }
    }
    ret
}

// upstream: drm_dumb_buffers.c drm_mode_mmap_dumb()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev`, `args`, and `file` must point to live DRM subsystem structures.
/// `args` must point to an aligned, mutable `DrmModeMapDumb` structure exclusively accessible.
pub unsafe extern "C" fn drm_mode_mmap_dumb(
    dev: *mut DrmDevice,
    args: *mut DrmModeMapDumb,
    file: *mut DrmFile,
) -> i32 {
    if dev.is_null() || args.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev`, `args`, and `file` are checked non-null above. The driver pointer
    // and its mapping callbacks are invoked with live pointers and exclusive access to `(*args).offset`.
    unsafe {
        let driver = (*dev).driver;
        if driver.is_null() || (*driver).dumb_create.is_none() {
            return -ENOSYS;
        }
        if let Some(map_offset) = (*driver).dumb_map_offset {
            map_offset(
                file,
                dev,
                (*args).handle,
                core::ptr::addr_of_mut!((*args).offset),
            )
        } else {
            crate::drm_gem_upstream::drm_gem_dumb_map_offset(
                file,
                dev,
                (*args).handle,
                core::ptr::addr_of_mut!((*args).offset),
            )
        }
    }
}

// upstream: drm_dumb_buffers.c drm_mode_mmap_dumb_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` and `file` must point to live DRM structures. If non-null, `data` must point to
/// a properly aligned, mutable `DrmModeMapDumb` structure exclusively accessible.
pub unsafe extern "C" fn drm_mode_mmap_dumb_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if data.is_null() {
        return -EINVAL;
    }
    let args = data.cast::<DrmModeMapDumb>();
    // SAFETY: `data` is non-null and typed as `DrmModeMapDumb` per caller contract.
    // Forwarded with valid `dev` and `file` pointers to `drm_mode_mmap_dumb`.
    let ret = unsafe { drm_mode_mmap_dumb(dev, args, file) };
    if ret != 0 {
        // SAFETY: On failure, `args` remains valid, non-null, and exclusively accessible to clear the offset.
        unsafe { (*args).offset = 0 };
    }
    ret
}

// upstream: drm_dumb_buffers.c drm_mode_destroy_dumb()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` and `file` must point to live, initialized DRM subsystem structures (`DrmDevice`, `DrmFile`).
pub unsafe extern "C" fn drm_mode_destroy_dumb(
    dev: *mut DrmDevice,
    handle: u32,
    file: *mut DrmFile,
) -> i32 {
    if dev.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` and `file` are checked non-null above and live per contract.
    // The driver pointer is checked before delegating handle deletion.
    unsafe {
        let driver = (*dev).driver;
        if driver.is_null() || (*driver).dumb_create.is_none() {
            return -ENOSYS;
        }
        crate::drm_gem_upstream::drm_gem_handle_delete(file, handle)
    }
}

// upstream: drm_dumb_buffers.c drm_mode_destroy_dumb_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` and `file` must point to live DRM structures. If non-null, `data` must point to
/// an aligned, readable `DrmModeDestroyDumb` structure.
pub unsafe extern "C" fn drm_mode_destroy_dumb_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if data.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is checked non-null and points to a readable `DrmModeDestroyDumb` per contract.
    // `dev` and `file` are forwarded as valid pointers.
    unsafe { drm_mode_destroy_dumb(dev, (*data.cast::<DrmModeDestroyDumb>()).handle, file) }
}
