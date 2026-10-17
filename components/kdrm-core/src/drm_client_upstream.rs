// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_client.c`.

#![allow(non_snake_case, non_upper_case_globals)]

use core::ffi::c_char;

#[cfg(not(test))]
use drm_kapi::mutex::{mutex_lock, mutex_unlock};
use drm_kapi::{
    config::{EINVAL, ENOENT, ENOMEM, ERR_PTR},
    list::{list_add_tail, list_del},
    memory::{kfree, kzalloc_obj},
};
#[cfg(test)]
/// # Safety
/// Test dummy lock helper.
unsafe fn mutex_lock<M>(_mutex: M) {}
#[cfg(test)]
/// # Safety
/// Test dummy unlock helper.
unsafe fn mutex_unlock<M>(_mutex: M) {}

use crate::{
    drm_client_types::{DrmClientBuffer, DrmClientDev},
    drm_device_types::DrmDevice,
    drm_file_types::DrmMinor,
    drm_framebuffer_types::DrmFramebufferFuncs,
    drm_gem_upstream::{
        drm_gem_lock, drm_gem_object_lookup, drm_gem_object_put, drm_gem_unlock, drm_gem_vmap,
        drm_gem_vmap_locked, drm_gem_vunmap, drm_gem_vunmap_locked,
    },
    drm_rect_types::DrmRect,
    iosys_map_types::IosysMap,
    uapi_drm_mode_types::DrmModeFbCmd2,
};

// drm_client.c: client_open()
/// # Safety
/// `client` must be non-null, properly aligned, and point to a live `DrmClientDev` whose `dev` points to a live `DrmDevice`.
unsafe fn drm_client_open(client: *mut DrmClientDev) -> i32 {
    // SAFETY: `client` is non-null and valid per caller safety contract.
    let dev = unsafe { (*client).dev };
    // SAFETY: `dev` is a live `DrmDevice` associated with `client`.
    let minor: *mut DrmMinor = unsafe { (*dev).primary };
    if minor.is_null() {
        return -EINVAL;
    }
    // SAFETY: `minor` was checked non-null and points to the primary minor device.
    let file = unsafe { crate::drm_file_upstream::drm_file_alloc(minor) };
    if file.is_null() {
        return -ENOMEM;
    }
    // SAFETY: `dev`, `file`, and `client` are live objects. `filelist_mutex` serializes
    // insertion of `file->lhead` into `dev->filelist_internal`.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).filelist_mutex));
        list_add_tail(
            core::ptr::addr_of_mut!((*file).lhead),
            core::ptr::addr_of_mut!((*dev).filelist_internal),
        );
        mutex_unlock(core::ptr::addr_of_mut!((*dev).filelist_mutex));
        (*client).file = file;
    }
    0
}

// drm_client.c: client_close()
/// # Safety
/// `client` must be non-null, properly aligned, and point to a live `DrmClientDev`.
unsafe fn drm_client_close(client: *mut DrmClientDev) {
    // SAFETY: `client` is valid per caller safety contract.
    let dev = unsafe { (*client).dev };
    // SAFETY: `client` is valid per caller safety contract.
    let file = unsafe { (*client).file };
    if file.is_null() {
        return;
    }
    // SAFETY: `file` was checked non-null and is registered in `dev->filelist_internal`.
    // `filelist_mutex` protects list removal before freeing `file`.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).filelist_mutex));
        list_del(core::ptr::addr_of_mut!((*file).lhead));
        mutex_unlock(core::ptr::addr_of_mut!((*dev).filelist_mutex));
        crate::drm_file_upstream::drm_file_free(file);
        (*client).file = core::ptr::null_mut();
    }
}

// upstream: drm_client.c drm_client_init()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to a live `DrmDevice`, `client` must point to allocated, writable storage for `DrmClientDev`,
/// `name` must be a valid null-terminated C string, and `funcs` must point to valid `DrmClientFuncs`.
pub unsafe extern "C" fn drm_client_init(
    dev: *mut DrmDevice,
    client: *mut DrmClientDev,
    name: *const c_char,
    funcs: *const crate::drm_client_types::DrmClientFuncs,
) -> i32 {
    if dev.is_null() || client.is_null() || name.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` and `client` are non-null and point to valid structures per caller contract.
    // Driver fields and dumb_create callback are checked before use.
    unsafe {
        if !crate::drm_mode_object_upstream::drm_core_check_feature(
            dev,
            crate::drm_mode_object_upstream::DRIVER_MODESET,
        ) || (*dev).driver.is_null()
            || (*(*dev).driver).dumb_create.is_none()
        {
            return -drm_kapi::config::EOPNOTSUPP;
        }
        (*client).dev = dev;
        (*client).name = name.cast_mut();
        (*client).funcs = funcs.cast_mut();

        let ret = crate::drm_client_modeset_upstream::drm_client_modeset_create(client);
        if ret != 0 {
            return ret;
        }
        let ret = drm_client_open(client);
        if ret != 0 {
            crate::drm_client_modeset_upstream::drm_client_modeset_free(client);
            return ret;
        }
        crate::drm_drv_upstream::drm_dev_get(dev);
        0
    }
}

// upstream: drm_client.c drm_client_register()
#[unsafe(no_mangle)]
/// # Safety
/// `client` must point to an initialized `DrmClientDev` with a valid `dev` pointer and valid callbacks.
pub unsafe extern "C" fn drm_client_register(client: *mut DrmClientDev) {
    // SAFETY: Caller guarantees `client` is either null or a valid pointer to `DrmClientDev`.
    if client.is_null() || unsafe { (*client).dev.is_null() } {
        return;
    }
    // SAFETY: `client` and its `dev` field were verified non-null above.
    let dev = unsafe { (*client).dev };
    // SAFETY: `dev` and `client` are valid. `clientlist_mutex` serializes adding `client` to `dev->clientlist`.
    // The optional hotplug callback is invoked while holding `clientlist_mutex`.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
        list_add_tail(
            core::ptr::addr_of_mut!((*client).list),
            core::ptr::addr_of_mut!((*dev).clientlist),
        );
        // Linux adds the client before the synchronous initial hotplug. This
        // ordering prevents a concurrent event from being lost.
        if let Some(hotplug) = (*client).funcs.as_ref().and_then(|f| f.hotplug) {
            let ret = hotplug(client);
            if ret != 0 {
                drm_kapi::print::drm_log_at(
                    drm_kapi::print::DrmLogLevel::Debug,
                    "drm",
                    file!(),
                    line!(),
                    &alloc::format!("client hotplug ret={ret}"),
                );
            }
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
    }
}

// upstream: drm_client.c drm_client_release()
#[unsafe(no_mangle)]
/// # Safety
/// `client` must point to a live `DrmClientDev` previously initialized by `drm_client_init`.
pub unsafe extern "C" fn drm_client_release(client: *mut DrmClientDev) {
    if client.is_null() {
        return;
    }
    // SAFETY: Caller guarantees `client` points to an initialized `DrmClientDev`.
    // Teardown helpers are called and device reference is dropped.
    unsafe {
        let dev = (*client).dev;
        if dev.is_null() {
            return;
        }
        crate::drm_client_modeset_upstream::drm_client_modeset_free(client);
        drm_client_close(client);
        if let Some(free) = (*client).funcs.as_ref().and_then(|f| f.free) {
            free(client);
        }
        crate::drm_drv_upstream::drm_dev_put(dev);
    }
}

// upstream: drm_client.c drm_client_buffer_delete()
#[unsafe(no_mangle)]
/// # Safety
/// `buffer` must point to a live `DrmClientBuffer` allocated by `drm_client_buffer_create` or be null.
pub unsafe extern "C" fn drm_client_buffer_delete(buffer: *mut DrmClientBuffer) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: Caller guarantees `buffer` points to an allocated `DrmClientBuffer` or is null.
    // Cleans up associated vmap, framebuffer, GEM object, and frees the buffer memory.
    unsafe {
        let fb = (*buffer).fb;
        let client = (*buffer).client;
        let gem = if fb.is_null() {
            (*buffer).gem
        } else {
            (*fb).obj[0]
        };
        if !gem.is_null() {
            drm_gem_vunmap(gem, core::ptr::addr_of_mut!((*buffer).map).cast());
        }
        if !fb.is_null() && !client.is_null() {
            let ret = crate::drm_framebuffer_upstream::drm_mode_rmfb(
                (*client).dev,
                (*fb).base.id,
                (*client).file,
            );
            if ret != 0 {
                drm_kapi::print::drm_log_at(
                    drm_kapi::print::DrmLogLevel::Error,
                    "drm",
                    file!(),
                    line!(),
                    &alloc::format!("Error removing FB:{} ({ret})", (*fb).base.id),
                );
            }
        }
        if !(*buffer).gem.is_null() {
            drm_gem_object_put((*buffer).gem);
        }
        kfree(buffer);
    }
}

// upstream: drm_client.c drm_client_buffer_create()
#[unsafe(no_mangle)]
/// # Safety
/// `client` must point to a live, registered `DrmClientDev` with valid `dev` and `file`.
pub unsafe extern "C" fn drm_client_buffer_create(
    client: *mut DrmClientDev,
    width: u32,
    height: u32,
    format: u32,
    handle: u32,
    pitch: u32,
) -> *mut DrmClientBuffer {
    // SAFETY: Caller guarantees `client` is either null or a valid pointer to `DrmClientDev`.
    if client.is_null() || unsafe { (*client).dev.is_null() || (*client).file.is_null() } {
        return ERR_PTR::<DrmClientBuffer>(-EINVAL);
    }
    let buffer = kzalloc_obj::<DrmClientBuffer>();
    if buffer.is_null() {
        return ERR_PTR::<DrmClientBuffer>(-ENOMEM);
    }
    // SAFETY: `buffer` was freshly allocated by `kzalloc_obj` and checked non-null.
    unsafe { (*buffer).client = client };

    // SAFETY: `client` is valid and `(*client).file` was verified non-null above.
    let obj = unsafe { drm_gem_object_lookup((*client).file, handle) };
    if obj.is_null() {
        // SAFETY: `buffer` was allocated above and has not been stored or exposed.
        unsafe { kfree(buffer) };
        return ERR_PTR::<DrmClientBuffer>(-ENOENT);
    }

    let mut request = DrmModeFbCmd2 {
        fb_id: 0,
        width,
        height,
        pixel_format: format,
        flags: 0,
        handles: [handle, 0, 0, 0],
        pitches: [pitch, 0, 0, 0],
        offsets: [0; 4],
        modifier: [0; 4],
    };
    // SAFETY: `client` is valid and live.
    let dev = unsafe { (*client).dev };
    // SAFETY: `client` is valid and live.
    let file = unsafe { (*client).file };
    // SAFETY: `dev` and `file` are valid Linux DRM objects; `request` is a local stack struct.
    let ret = unsafe {
        crate::drm_framebuffer_upstream::drm_mode_addfb2(
            dev,
            (&mut request as *mut DrmModeFbCmd2).cast(),
            file,
        )
    };
    if ret != 0 {
        // SAFETY: `obj` was looked up and `buffer` was allocated above; both are cleaned up on error.
        unsafe {
            drm_gem_object_put(obj);
            kfree(buffer);
        }
        return ERR_PTR::<DrmClientBuffer>(ret);
    }

    // SAFETY: `dev` and `file` are valid; looks up newly created framebuffer by ID.
    let fb = unsafe {
        crate::drm_framebuffer_upstream::drm_framebuffer_lookup(dev, file, request.fb_id)
    };
    if fb.is_null() {
        // SAFETY: Cleans up framebuffer registration, drops GEM reference, and frees buffer on failure.
        unsafe {
            crate::drm_framebuffer_upstream::drm_mode_rmfb(dev, request.fb_id, file);
            drm_gem_object_put(obj);
            kfree(buffer);
        }
        return ERR_PTR::<DrmClientBuffer>(-ENOENT);
    }
    // SAFETY: Drops lookup reference count on `fb`, initializes `comm` and records `gem` and `fb` in `buffer`.
    unsafe {
        crate::drm_framebuffer_upstream::drm_framebuffer_put(fb);
        copy_comm((*fb).comm.as_mut_ptr(), (*client).name);
        (*buffer).gem = obj;
        (*buffer).fb = fb;
    }
    buffer
}

/// # Safety
/// `dst` must point to a writable buffer of at least 16 bytes. `src` must point to a readable, null-terminated C string or at least 16 readable bytes.
unsafe fn copy_comm(dst: *mut c_char, src: *const c_char) {
    if dst.is_null() || src.is_null() {
        return;
    }
    // SAFETY: `dst` and `src` were checked non-null and caller guarantees `dst` has at least 16 bytes.
    unsafe {
        for i in 0..16 {
            let ch = *src.add(i);
            *dst.add(i) = ch;
            if ch == 0 {
                break;
            }
        }
        *dst.add(15) = 0;
    }
}

// upstream: drm_client.c drm_client_buffer_vmap_local()
#[unsafe(no_mangle)]
/// # Safety
/// `buffer` must point to a live `DrmClientBuffer` with a valid framebuffer; `map_copy` must point to writable storage for `IosysMap`.
pub unsafe extern "C" fn drm_client_buffer_vmap_local(
    buffer: *mut DrmClientBuffer,
    map_copy: *mut IosysMap,
) -> i32 {
    // SAFETY: Checks nullness of `buffer->fb`.
    if buffer.is_null() || map_copy.is_null() || unsafe { (*buffer).fb.is_null() } {
        return -EINVAL;
    }
    // SAFETY: `buffer`, `buffer->fb`, and `map_copy` are valid pointers. Acquires gem lock,
    // calls `drm_gem_vmap_locked`, and copies map into `map_copy`.
    unsafe {
        let gem = (*(*buffer).fb).obj[0];
        drm_gem_lock(gem);
        let ret = drm_gem_vmap_locked(gem, core::ptr::addr_of_mut!((*buffer).map).cast());
        if ret != 0 {
            drm_gem_unlock(gem);
            return ret;
        }
        core::ptr::copy_nonoverlapping(core::ptr::addr_of!((*buffer).map), map_copy, 1);
    }
    0
}

// upstream: drm_client.c drm_client_buffer_vunmap_local()
#[unsafe(no_mangle)]
/// # Safety
/// `buffer` must point to a live `DrmClientBuffer` with a valid framebuffer previously mapped with `drm_client_buffer_vmap_local`.
pub unsafe extern "C" fn drm_client_buffer_vunmap_local(buffer: *mut DrmClientBuffer) {
    // SAFETY: Checks nullness of `buffer->fb`.
    if buffer.is_null() || unsafe { (*buffer).fb.is_null() } {
        return;
    }
    // SAFETY: `buffer` and `buffer->fb` are valid pointers. Unmaps locked buffer and releases lock.
    unsafe {
        let gem = (*(*buffer).fb).obj[0];
        drm_gem_vunmap_locked(gem, core::ptr::addr_of_mut!((*buffer).map).cast());
        drm_gem_unlock(gem);
    }
}

// upstream: drm_client.c drm_client_buffer_vmap()
#[unsafe(no_mangle)]
/// # Safety
/// `buffer` must point to a live `DrmClientBuffer` with a valid framebuffer; `map_copy` must point to writable storage for `IosysMap`.
pub unsafe extern "C" fn drm_client_buffer_vmap(
    buffer: *mut DrmClientBuffer,
    map_copy: *mut IosysMap,
) -> i32 {
    // SAFETY: Checks nullness of `buffer->fb`.
    if buffer.is_null() || map_copy.is_null() || unsafe { (*buffer).fb.is_null() } {
        return -EINVAL;
    }
    // SAFETY: `buffer` and `buffer->fb` are valid pointers; calls `drm_gem_vmap` and copies map into `map_copy`.
    unsafe {
        let ret = drm_gem_vmap(
            (*(*buffer).fb).obj[0],
            core::ptr::addr_of_mut!((*buffer).map).cast(),
        );
        if ret != 0 {
            return ret;
        }
        core::ptr::copy_nonoverlapping(core::ptr::addr_of!((*buffer).map), map_copy, 1);
    }
    0
}

// upstream: drm_client.c drm_client_buffer_vunmap()
#[unsafe(no_mangle)]
/// # Safety
/// `buffer` must point to a live `DrmClientBuffer` with a valid framebuffer previously mapped with `drm_client_buffer_vmap`.
pub unsafe extern "C" fn drm_client_buffer_vunmap(buffer: *mut DrmClientBuffer) {
    // SAFETY: Checks nullness of `buffer->fb`.
    if buffer.is_null() || unsafe { (*buffer).fb.is_null() } {
        return;
    }
    // SAFETY: `buffer` and `buffer->fb` are valid pointers; calls `drm_gem_vunmap`.
    unsafe {
        drm_gem_vunmap(
            (*(*buffer).fb).obj[0],
            core::ptr::addr_of_mut!((*buffer).map).cast(),
        );
    }
}

// upstream: drm_client.c drm_client_buffer_create_dumb()
#[unsafe(no_mangle)]
/// # Safety
/// `client` must point to a live `DrmClientDev` with initialized `dev` and `file`.
pub unsafe extern "C" fn drm_client_buffer_create_dumb(
    client: *mut DrmClientDev,
    width: u32,
    height: u32,
    format: u32,
) -> *mut DrmClientBuffer {
    // SAFETY: Checks nullness of `client->dev`.
    if client.is_null() || unsafe { (*client).dev.is_null() } {
        return ERR_PTR::<DrmClientBuffer>(-EINVAL);
    }
    let info = crate::drm_fourcc_upstream::drm_format_info(format);
    if info.is_null() {
        return ERR_PTR::<DrmClientBuffer>(-EINVAL);
    }
    let mut args = crate::uapi_drm_mode_types::DrmModeCreateDumb {
        height,
        width,
        // SAFETY: `info` was verified non-null above.
        bpp: unsafe { crate::drm_fourcc_upstream::drm_format_info_bpp(info, 0) },
        flags: 0,
        handle: 0,
        pitch: 0,
        size: 0,
    };
    // SAFETY: `client` and `client->dev` were verified non-null above.
    let dev = unsafe { (*client).dev };
    // SAFETY: `client` was verified non-null above.
    let file = unsafe { (*client).file };
    // SAFETY: `dev` and `file` are valid; `args` is a local stack structure.
    let ret = unsafe {
        crate::drm_dumb_buffers_upstream::drm_mode_create_dumb(
            dev,
            (&mut args as *mut crate::uapi_drm_mode_types::DrmModeCreateDumb).cast(),
            file,
        )
    };
    if ret != 0 {
        return ERR_PTR::<DrmClientBuffer>(ret);
    }
    // SAFETY: `client` is valid; creates client buffer wrapping the dumb handle.
    let buffer =
        unsafe { drm_client_buffer_create(client, width, height, format, args.handle, args.pitch) };
    if drm_kapi::config::IS_ERR(buffer) {
        // SAFETY: Destroys the allocated dumb buffer on buffer creation failure.
        unsafe {
            crate::drm_dumb_buffers_upstream::drm_mode_destroy_dumb(dev, args.handle, file);
        }
        return buffer;
    }
    // SAFETY: Destroys the dumb handle reference since drm_client_buffer_create already created/acquired FB.
    unsafe { crate::drm_dumb_buffers_upstream::drm_mode_destroy_dumb(dev, args.handle, file) };
    buffer
}

// upstream: drm_client.c drm_client_buffer_flush()
#[unsafe(no_mangle)]
/// # Safety
/// `buffer` must point to a live `DrmClientBuffer` with a valid framebuffer; if non-null, `rect` must point to a valid `DrmRect`.
pub unsafe extern "C" fn drm_client_buffer_flush(
    buffer: *mut DrmClientBuffer,
    rect: *mut DrmRect,
) -> i32 {
    // SAFETY: Checks nullness of `buffer->fb` and `buffer->fb->funcs`.
    if buffer.is_null() || unsafe { (*buffer).fb.is_null() || (*(*buffer).fb).funcs.is_null() } {
        return 0;
    }
    // SAFETY: `buffer`, `buffer->fb`, and `funcs` were validated non-null above. Calls `dirty` callback.
    unsafe {
        let funcs: *mut DrmFramebufferFuncs = (*(*buffer).fb).funcs;
        let Some(dirty) = (*funcs).dirty else {
            return 0;
        };
        if rect.is_null() {
            return dirty(
                (*buffer).fb,
                (*(*buffer).client).file,
                0,
                0,
                core::ptr::null_mut(),
                0,
            );
        }
        let clip = crate::uapi_drm_types::DrmClipRect {
            x1: (*rect).x1 as u16,
            y1: (*rect).y1 as u16,
            x2: (*rect).x2 as u16,
            y2: (*rect).y2 as u16,
        };
        dirty(
            (*buffer).fb,
            (*(*buffer).client).file,
            0,
            0,
            (&clip as *const crate::uapi_drm_types::DrmClipRect).cast_mut(),
            1,
        )
    }
}
