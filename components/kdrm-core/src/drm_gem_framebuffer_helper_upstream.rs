// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
//! GEM-backed framebuffers.
//!
//! A framebuffer built here references one GEM object per plane. Creation looks
//! up each handle, checks that the object is large enough for the plane
//! geometry, and then hands the references to the framebuffer, which drops them
//! again on destruction. The module also provides per-plane geometry, CPU access
//! bracketing for dma-buf imports, vmap bookkeeping, and AFBC sizing.

use core::{
    ffi::{c_int, c_void},
    mem::size_of,
    ptr::null_mut,
};

use drm_kapi::{
    assert::warn_on,
    config::{EINVAL, ENOENT, ENOMEM, GFP_KERNEL, err_ptr},
    memory::{kfree, kzalloc},
    print::{DrmLogLevel, drm_log_at},
};

use crate::{
    drm_color_mgmt_upstream::drm_drv_uses_atomic_modeset,
    drm_damage_helper_upstream::drm_atomic_helper_dirtyfb,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_fourcc_types::DrmFormatInfo,
    drm_fourcc_upstream::{drm_format_info_bpp, drm_format_info_min_pitch},
    drm_framebuffer_types::{
        DRM_FORMAT_MAX_PLANES, DrmAfbcFramebuffer, DrmFramebuffer, DrmFramebufferFuncs,
    },
    drm_framebuffer_upstream::{drm_framebuffer_cleanup, drm_framebuffer_init},
    drm_gem_types::DrmGemObject,
    drm_gem_upstream::{
        drm_gem_handle_create, drm_gem_object_lookup, drm_gem_object_put, drm_gem_vmap,
        drm_gem_vunmap,
    },
    drm_modeset_helper_upstream::drm_helper_mode_fill_fb_struct,
    drm_plane_upstream::drm_any_plane_has_format,
    drm_prime_upstream::{DmaBuf, DmaBufAttachment},
    iosys_map_types::IosysMap,
    uapi_drm_consts::*,
    uapi_drm_mode_types::DrmModeFbCmd2,
    uapi_drm_types::DrmClipRect,
};

unsafe extern "C" {
    fn dma_buf_begin_cpu_access(dmabuf: *mut DmaBuf, direction: c_int) -> c_int;
    fn dma_buf_end_cpu_access(dmabuf: *mut DmaBuf, direction: c_int) -> c_int;
}

const LOG_TAG: &str = "drm";

// AFBC layout constants. Only the 16x16 and 32x8 superblock modes are accepted.
const AFBC_HEADER_BYTES_PER_BLOCK: u64 = 16;
const AFBC_PIXELS_PER_BLOCK: u64 = 256;
const AFBC_BODY_ALIGN: u64 = 128;
const AFBC_PLAIN_HEADER_ALIGN: u64 = 64;
const AFBC_TILED_HEADER_ALIGN: u64 = 4096;
const AFBC_TILED_SCALE: u64 = 8;
const AFBC_BLOCK_SIZE_MASK: u64 = 0xf;
const AFBC_BLOCK_SIZE_16X16: u64 = 1;
const AFBC_BLOCK_SIZE_32X8: u64 = 2;
const AFBC_TILED_BIT: u64 = 1 << 8;

fn debug_log(message: &str) {
    drm_log_at(DrmLogLevel::Debug, LOG_TAG, file!(), line!(), message);
}

fn error_log(message: &str) {
    drm_log_at(DrmLogLevel::Error, LOG_TAG, file!(), line!(), message);
}

/// Rounds `value` up to the next multiple of `align` (a power of two).
const fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

/// Returns `true` when the mapping has no address in the active variant.
///
/// # Safety
/// A non-null `map` must point at a readable [`IosysMap`].
unsafe fn iosys_map_is_null(map: *const IosysMap) -> bool {
    if map.is_null() {
        return true;
    }
    // SAFETY: `map` is non-null and readable per the caller contract. Both union views share storage, so the pointer bits are read from whichever variant is active.
    unsafe {
        if (*map).is_iomem {
            (*map)._anon_0.vaddr_iomem.is_null()
        } else {
            (*map)._anon_0.vaddr.is_null()
        }
    }
}

/// Advances a mapping by `bytes`, honouring whether it refers to I/O memory.
///
/// # Safety
/// `map` must point at a valid [`IosysMap`] whose active address is valid for
/// `bytes` bytes of forward arithmetic.
pub(crate) unsafe fn iosys_map_incr(map: *mut IosysMap, bytes: usize) {
    // SAFETY: `map` is valid per the caller contract; only the active union variant is updated.
    unsafe {
        if (*map).is_iomem {
            let base = (*map)._anon_0.vaddr_iomem.cast::<u8>();
            (*map)._anon_0.vaddr_iomem = base.add(bytes).cast();
        } else {
            let base = (*map)._anon_0.vaddr.cast::<u8>();
            (*map)._anon_0.vaddr = base.add(bytes).cast();
        }
    }
}

/// Reports whether a GEM object is a dma-buf import that needs CPU access
/// bracketing.
///
/// # Safety
/// `obj` must point at a live GEM object.
unsafe fn is_imported(obj: *mut DrmGemObject) -> bool {
    // SAFETY: `obj` is live per the caller contract, so reading its attachment pointer is valid.
    unsafe { !(*obj).import_attach.is_null() }
}

/// Shared implementation of the per-plane geometry helpers. Plane 0 keeps the
/// full extent. Later planes are divided by the matching subsampling factor,
/// rounding up. A zero factor means no subsampling.
///
/// # Safety
/// `info` must be null or point at a valid [`DrmFormatInfo`].
unsafe fn plane_extent(
    info: *const DrmFormatInfo,
    extent: u32,
    plane: u32,
    subsampling: impl Fn(&DrmFormatInfo) -> u8,
) -> u32 {
    if info.is_null() {
        return 0;
    }
    // SAFETY: `info` is non-null and valid per the caller contract.
    let info = unsafe { &*info };
    if plane >= u32::from(info.num_planes) {
        return 0;
    }
    if plane == 0 {
        return extent;
    }
    match u32::from(subsampling(info)) {
        0 => extent,
        factor => extent.div_ceil(factor),
    }
}

/// Width in pixels of `plane` for a buffer that is `width` pixels wide.
///
/// Returns 0 for a null format or an out-of-range plane.
///
/// # Safety
/// `info` must be null or point at a valid [`DrmFormatInfo`].
pub unsafe fn drm_format_info_plane_width(
    info: *const DrmFormatInfo,
    width: u32,
    plane: u32,
) -> u32 {
    // SAFETY: forwarded unchanged; `info` meets the caller contract above.
    unsafe { plane_extent(info, width, plane, |f| f.hsub) }
}

/// Height in rows of `plane` for a buffer that is `height` rows tall.
///
/// Returns 0 for a null format or an out-of-range plane.
///
/// # Safety
/// `info` must be null or point at a valid [`DrmFormatInfo`].
pub unsafe fn drm_format_info_plane_height(
    info: *const DrmFormatInfo,
    height: u32,
    plane: u32,
) -> u32 {
    // SAFETY: forwarded unchanged; `info` meets the caller contract above.
    unsafe { plane_extent(info, height, plane, |f| f.vsub) }
}

/// Number of planes in the framebuffer's format, capped at the framebuffer
/// array size. Returns 0 when the format is not set.
///
/// # Safety
/// `fb` must point at a valid framebuffer.
unsafe fn fb_plane_count(fb: *const DrmFramebuffer) -> usize {
    // SAFETY: `fb` is valid per the caller contract.
    let format = unsafe { (*fb).format };
    if format.is_null() {
        return 0;
    }
    // SAFETY: a non-null format pointer refers to a live format description.
    let planes = unsafe { usize::from((*format).num_planes) };
    planes.min(DRM_FORMAT_MAX_PLANES)
}

/// Returns the GEM object backing `plane` of `fb`, or null (with a warning) if
/// the plane index is out of range or no object is attached.
///
/// # Safety
/// `fb` must be null or point at a valid framebuffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_get_obj(
    fb: *mut DrmFramebuffer,
    plane: u32,
) -> *mut DrmGemObject {
    if fb.is_null() {
        return null_mut();
    }
    // SAFETY: `fb` is non-null and valid per the caller contract.
    let framebuffer = unsafe { &*fb };
    if warn_on(framebuffer.dev.is_null() || plane as usize >= framebuffer.obj.len()) {
        return null_mut();
    }
    let obj = framebuffer.obj[plane as usize];
    if warn_on(obj.is_null()) {
        return null_mut();
    }
    obj
}

/// Fills in the framebuffer from the mode command and attaches the plane
/// objects, then registers it with the device. The framebuffer takes over the
/// references in `objs` on success; on failure they stay with the caller.
///
/// # Safety
/// All pointers must be valid for their use: `dev` and `fb` live, `info` and
/// `mode_cmd` readable, and `funcs` a valid callback table.
unsafe fn initialise_framebuffer(
    dev: *mut DrmDevice,
    fb: *mut DrmFramebuffer,
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
    objs: &[*mut DrmGemObject],
    funcs: *const DrmFramebufferFuncs,
) -> i32 {
    // SAFETY: the pointers satisfy the contract above; `objs` provides exactly one slot per attached plane.
    unsafe {
        drm_helper_mode_fill_fb_struct(dev, fb, info, mode_cmd);
        for (slot, obj) in (*fb).obj.iter_mut().zip(objs) {
            *slot = *obj;
        }
        let ret = drm_framebuffer_init(dev, fb, funcs);
        if ret != 0 {
            error_log(&alloc::format!("Failed to init framebuffer: {ret}"));
        }
        ret
    }
}

/// Drops one reference from each object in `objs`.
///
/// # Safety
/// Every entry must be a live GEM object reference owned by the caller.
unsafe fn release_objects(objs: &[*mut DrmGemObject]) {
    for &obj in objs {
        // SAFETY: each entry is a reference owned by the caller, per the contract above.
        unsafe { drm_gem_object_put(obj) };
    }
}

/// Looks up the GEM object for every plane of `mode_cmd` and checks that each
/// one holds at least the bytes the plane layout needs.
///
/// On success the first `Ok(n)` slots of `objs` hold references owned by the
/// caller. On failure, every reference taken so far has already been dropped.
///
/// # Safety
/// `file` must be a live DRM file, and `info` and `mode_cmd` must be valid.
unsafe fn lookup_plane_objects(
    file: *mut DrmFile,
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
    objs: &mut [*mut DrmGemObject; DRM_FORMAT_MAX_PLANES],
) -> Result<usize, i32> {
    // SAFETY: `info` and `mode_cmd` are valid per the caller contract.
    let (format, cmd) = unsafe { (&*info, &*mode_cmd) };
    let planes = usize::from(format.num_planes);
    if planes > DRM_FORMAT_MAX_PLANES {
        return Err(-EINVAL);
    }

    for plane in 0..planes {
        // SAFETY: `info` is a valid format description per the caller contract.
        let width = unsafe { drm_format_info_plane_width(info, cmd.width, plane as u32) };
        // SAFETY: as above; the same format description provides the plane height.
        let height = unsafe { drm_format_info_plane_height(info, cmd.height, plane as u32) };
        // SAFETY: `file` is live per the caller contract; a successful lookup returns a reference that is recorded below.
        let obj = unsafe { drm_gem_object_lookup(file, cmd.handles[plane]) };
        if obj.is_null() {
            debug_log("Failed to lookup GEM object");
            // SAFETY: the first `plane` slots were filled by earlier iterations and each holds a reference taken here.
            unsafe { release_objects(&objs[..plane]) };
            return Err(-ENOENT);
        }
        objs[plane] = obj;

        // SAFETY: `obj` is a live reference taken above.
        let available = unsafe { (*obj).size } as u64;
        let pitch = u64::from(cmd.pitches[plane]);
        let min_pitch = unsafe { drm_format_info_min_pitch(info, plane as i32, width) };
        let rows = u64::from(height.saturating_sub(1));
        let required = rows
            .saturating_mul(pitch)
            .saturating_add(min_pitch)
            .saturating_add(u64::from(cmd.offsets[plane]));

        if available < required {
            debug_log(&alloc::format!(
                "GEM object size ({available}) smaller than minimum size ({required}) for plane \
                 {plane}"
            ));
            // SAFETY: `obj` is the reference taken above and is dropped exactly once here.
            unsafe { drm_gem_object_put(obj) };
            objs[plane] = null_mut();
            // SAFETY: slots `0..plane` hold references taken by earlier iterations.
            unsafe { release_objects(&objs[..plane]) };
            return Err(-EINVAL);
        }
    }
    Ok(planes)
}

/// Destroys a GEM-backed framebuffer: drops the plane references, runs the core
/// cleanup, and frees the framebuffer. Null is ignored.
///
/// # Safety
/// `fb` must be a framebuffer created by this module that has no remaining
/// users and is not freed elsewhere.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_destroy(fb: *mut DrmFramebuffer) {
    if fb.is_null() {
        return;
    }
    // SAFETY: `fb` is valid and unused per the caller contract; each plane reference is dropped exactly once before the framebuffer is freed.
    unsafe {
        let planes = fb_plane_count(fb);
        for &obj in (*fb).obj.iter().take(planes) {
            drm_gem_object_put(obj);
        }
        drm_framebuffer_cleanup(fb);
        kfree(fb);
    }
}

/// Exposes the first plane's GEM object as a handle on `file`.
///
/// # Safety
/// `fb` must be a valid framebuffer, `file` a live DRM file, and `handle`
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_create_handle(
    fb: *mut DrmFramebuffer,
    file: *mut DrmFile,
    handle: *mut u32,
) -> i32 {
    // SAFETY: the caller contract above covers `fb`, `file`, and `handle`.
    unsafe { drm_gem_handle_create(file, (*fb).obj[0], handle) }
}

/// Builds a GEM-backed framebuffer in an existing allocation.
///
/// Returns 0 on success. On failure no framebuffer is registered and every
/// object reference taken here has been released.
///
/// # Safety
/// `dev`, `fb`, `file` must be live. `info` and `mode_cmd` must be valid, and
/// `funcs` must be a valid callback table for the framebuffer's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_init_with_funcs(
    dev: *mut DrmDevice,
    fb: *mut DrmFramebuffer,
    file: *mut DrmFile,
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
    funcs: *const DrmFramebufferFuncs,
) -> i32 {
    // SAFETY: the caller contract above covers every pointer dereferenced here.
    unsafe {
        let cmd = &*mode_cmd;
        if drm_drv_uses_atomic_modeset(dev)
            && !drm_any_plane_has_format(dev, cmd.pixel_format, cmd.modifier[0])
        {
            debug_log(&alloc::format!(
                "Unsupported pixel format {} / modifier {:#x}",
                cmd.pixel_format,
                cmd.modifier[0]
            ));
            return -EINVAL;
        }

        let mut objs = [null_mut(); DRM_FORMAT_MAX_PLANES];
        let attached = match lookup_plane_objects(file, info, mode_cmd, &mut objs) {
            Ok(count) => count,
            Err(err) => return err,
        };

        let ret = initialise_framebuffer(dev, fb, info, mode_cmd, &objs[..attached], funcs);
        if ret != 0 {
            release_objects(&objs[..attached]);
        }
        ret
    }
}

/// Allocates and initialises a GEM-backed framebuffer with the given callbacks.
///
/// Returns the new framebuffer, or an error pointer (`-ENOMEM` or the
/// initialisation error).
///
/// # Safety
/// Same requirements as [`drm_gem_fb_init_with_funcs`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_create_with_funcs(
    dev: *mut DrmDevice,
    file: *mut DrmFile,
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
    funcs: *const DrmFramebufferFuncs,
) -> *mut DrmFramebuffer {
    // SAFETY: the allocation is checked before use, and on initialisation failure it is freed exactly once.
    unsafe {
        let fb = kzalloc(size_of::<DrmFramebuffer>(), GFP_KERNEL).cast::<DrmFramebuffer>();
        if fb.is_null() {
            return err_ptr(-ENOMEM);
        }
        let ret = drm_gem_fb_init_with_funcs(dev, fb, file, info, mode_cmd, funcs);
        if ret != 0 {
            kfree(fb.cast::<c_void>());
            return err_ptr(ret);
        }
        fb
    }
}

/// Callback table without a dirty handler.
static DRM_GEM_FB_FUNCS: DrmFramebufferFuncs = DrmFramebufferFuncs {
    destroy: Some(drm_gem_fb_destroy),
    create_handle: Some(drm_gem_fb_create_handle),
    dirty: None,
};

/// Creates a GEM-backed framebuffer without a dirty handler.
///
/// # Safety
/// Same requirements as [`drm_gem_fb_init_with_funcs`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_create(
    dev: *mut DrmDevice,
    file: *mut DrmFile,
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
) -> *mut DrmFramebuffer {
    // SAFETY: forwarded to the generic constructor; the static callback table lives for the whole program.
    unsafe { drm_gem_fb_create_with_funcs(dev, file, info, mode_cmd, &DRM_GEM_FB_FUNCS) }
}

/// Dirty callback that forwards to the atomic damage helper.
///
/// # Safety
/// Same requirements as `drm_atomic_helper_dirtyfb`.
unsafe extern "C" fn drm_gem_fb_dirtyfb(
    fb: *mut DrmFramebuffer,
    file_priv: *mut DrmFile,
    flags: u32,
    color: u32,
    clips: *mut DrmClipRect,
    num_clips: u32,
) -> i32 {
    // SAFETY: the arguments are passed through unchanged, so the damage helper's contract is the caller's.
    unsafe { drm_atomic_helper_dirtyfb(fb, file_priv, flags, color, clips, num_clips) }
}

/// Callback table with the damage-tracking dirty handler.
static DRM_GEM_FB_FUNCS_DIRTYFB: DrmFramebufferFuncs = DrmFramebufferFuncs {
    destroy: Some(drm_gem_fb_destroy),
    create_handle: Some(drm_gem_fb_create_handle),
    dirty: Some(drm_gem_fb_dirtyfb),
};

/// Creates a GEM-backed framebuffer that supports dirty-rectangle updates.
///
/// # Safety
/// Same requirements as [`drm_gem_fb_init_with_funcs`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_create_with_dirty(
    dev: *mut DrmDevice,
    file: *mut DrmFile,
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
) -> *mut DrmFramebuffer {
    // SAFETY: forwarded to the generic constructor; the static callback table lives for the whole program.
    unsafe { drm_gem_fb_create_with_funcs(dev, file, info, mode_cmd, &DRM_GEM_FB_FUNCS_DIRTYFB) }
}

/// Maps every plane's GEM object. On success, `data` (if non-null) receives
/// the per-plane addresses, advanced by the plane offsets. On failure, planes
/// already mapped are unmapped again.
///
/// # Safety
/// `fb` must be valid. `map` must have room for every plane, and `data`, when
/// non-null, must also have room for every plane.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_vmap(
    fb: *mut DrmFramebuffer,
    map: *mut IosysMap,
    data: *mut IosysMap,
) -> i32 {
    // SAFETY: `fb` and the map arrays satisfy the caller contract; plane indices are bounded by the format's plane count.
    unsafe {
        let planes = fb_plane_count(fb);
        let mut mapped = 0;
        let mut ret = 0;
        while mapped < planes {
            let obj = drm_gem_fb_get_obj(fb, mapped as u32);
            if obj.is_null() {
                ret = -EINVAL;
                break;
            }
            ret = drm_gem_vmap(obj, map.add(mapped).cast());
            if ret != 0 {
                break;
            }
            mapped += 1;
        }

        if ret != 0 {
            for plane in (0..mapped).rev() {
                let obj = drm_gem_fb_get_obj(fb, plane as u32);
                if !obj.is_null() {
                    drm_gem_vunmap(obj, map.add(plane).cast());
                }
            }
            return ret;
        }

        if !data.is_null() {
            for plane in 0..planes {
                core::ptr::copy_nonoverlapping(map.add(plane), data.add(plane), 1);
                if iosys_map_is_null(data.add(plane)) {
                    continue;
                }
                iosys_map_incr(data.add(plane), (*fb).offsets[plane] as usize);
            }
        }
        0
    }
}

/// Unmaps the planes previously mapped by [`drm_gem_fb_vmap`]. Planes whose
/// mapping is already empty are skipped.
///
/// # Safety
/// `fb` must be valid and `map` must be the array filled by `drm_gem_fb_vmap`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_vunmap(fb: *mut DrmFramebuffer, map: *mut IosysMap) {
    // SAFETY: `fb` and `map` satisfy the caller contract, and plane indices are bounded by the format's plane count.
    unsafe {
        for plane in (0..fb_plane_count(fb)).rev() {
            let obj = drm_gem_fb_get_obj(fb, plane as u32);
            if obj.is_null() || iosys_map_is_null(map.add(plane)) {
                continue;
            }
            drm_gem_vunmap(obj, map.add(plane).cast());
        }
    }
}

/// Ends CPU access for the first `planes` planes that are dma-buf imports, in
/// reverse order. Errors are logged and do not stop the loop.
///
/// # Safety
/// `fb` must be a valid framebuffer and `planes` must not exceed its plane count.
unsafe fn end_cpu_access_planes(fb: *mut DrmFramebuffer, dir: c_int, planes: usize) {
    for plane in (0..planes).rev() {
        // SAFETY: `fb` is valid per the caller contract; imported objects have a live attachment and dma-buf for as long as the framebuffer holds them.
        unsafe {
            let obj = drm_gem_fb_get_obj(fb, plane as u32);
            if obj.is_null() || !is_imported(obj) {
                continue;
            }
            let attach = (*obj).import_attach.cast::<DmaBufAttachment>();
            let ret = dma_buf_end_cpu_access((*attach).dmabuf, dir);
            if ret != 0 {
                error_log(&alloc::format!(
                    "dma_buf_end_cpu_access({plane}, {dir}) failed: {ret}"
                ));
            }
        }
    }
}

/// Begins CPU access on every dma-buf-imported plane. If one plane fails, the
/// planes that were already begun are ended again.
///
/// # Safety
/// `fb` must be a valid framebuffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_begin_cpu_access(fb: *mut DrmFramebuffer, dir: c_int) -> i32 {
    // SAFETY: `fb` is valid per the caller contract; imported planes carry a live attachment.
    unsafe {
        let planes = fb_plane_count(fb);
        let mut begun = 0;
        let mut ret = 0;
        while begun < planes {
            let obj = drm_gem_fb_get_obj(fb, begun as u32);
            if obj.is_null() {
                ret = -EINVAL;
                break;
            }
            if is_imported(obj) {
                let attach = (*obj).import_attach.cast::<DmaBufAttachment>();
                ret = dma_buf_begin_cpu_access((*attach).dmabuf, dir);
                if ret != 0 {
                    break;
                }
            }
            begun += 1;
        }

        if ret != 0 {
            end_cpu_access_planes(fb, dir, begun);
            return ret;
        }
        0
    }
}

/// Ends CPU access on every dma-buf-imported plane of `fb`.
///
/// # Safety
/// `fb` must be a valid framebuffer whose access was begun with
/// [`drm_gem_fb_begin_cpu_access`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_end_cpu_access(fb: *mut DrmFramebuffer, dir: c_int) {
    // SAFETY: `fb` is valid per the caller contract.
    unsafe { end_cpu_access_planes(fb, dir, fb_plane_count(fb)) };
}

/// Bytes per pixel used for the AFBC body, with special cases for formats
/// whose packing is not a whole number of bits per pixel.
///
/// # Safety
/// `info` must point at a valid [`DrmFormatInfo`].
unsafe fn afbc_bits_per_pixel(info: *const DrmFormatInfo) -> u32 {
    // SAFETY: `info` is valid per the caller contract.
    match unsafe { (*info).format } {
        DRM_FORMAT_YUV420_8BIT => 12,
        DRM_FORMAT_YUV420_10BIT => 15,
        DRM_FORMAT_VUY101010 => 30,
        // SAFETY: `info` is valid per the caller contract.
        _ => unsafe { drm_format_info_bpp(info, 0) },
    }
}

/// Computes the AFBC superblock geometry and total byte size for `mode_cmd`,
/// storing the results in `afbc_fb`.
///
/// Returns 0 on success, or `-EINVAL` for an unsupported block-size modifier,
/// a zero bits-per-pixel format, or a size that does not fit in 32 bits.
///
/// # Safety
/// `info` and `mode_cmd` must be valid, and `afbc_fb` must be writable.
unsafe fn afbc_layout(
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
    afbc_fb: *mut DrmAfbcFramebuffer,
) -> i32 {
    // SAFETY: `mode_cmd` and `afbc_fb` are valid per the caller contract.
    let (cmd, out) = unsafe { (&*mode_cmd, &mut *afbc_fb) };
    let modifier = cmd.modifier[0];

    let (block_w, block_h) = match modifier & AFBC_BLOCK_SIZE_MASK {
        AFBC_BLOCK_SIZE_16X16 => (16u64, 16u64),
        AFBC_BLOCK_SIZE_32X8 => (32u64, 8u64),
        other => {
            debug_log(&alloc::format!(
                "Invalid AFBC_FORMAT_MOD_BLOCK_SIZE: {other}.\n"
            ));
            return -EINVAL;
        }
    };
    out.block_width = block_w as u32;
    out.block_height = block_h as u32;

    let tiled = modifier & AFBC_TILED_BIT != 0;
    let (align_w, align_h, header_align) = if tiled {
        (
            block_w * AFBC_TILED_SCALE,
            block_h * AFBC_TILED_SCALE,
            AFBC_TILED_HEADER_ALIGN,
        )
    } else {
        (block_w, block_h, AFBC_PLAIN_HEADER_ALIGN)
    };

    let aligned_w = align_up(u64::from(cmd.width), align_w);
    let aligned_h = align_up(u64::from(cmd.height), align_h);
    let (Ok(aligned_w32), Ok(aligned_h32)) = (u32::try_from(aligned_w), u32::try_from(aligned_h))
    else {
        return -EINVAL;
    };
    out.aligned_width = aligned_w32;
    out.aligned_height = aligned_h32;
    out.offset = cmd.offsets[0];

    // SAFETY: `info` is valid per the caller contract.
    let bpp = u64::from(unsafe { afbc_bits_per_pixel(info) });
    if bpp == 0 {
        debug_log("Invalid AFBC bpp value: 0\n");
        return -EINVAL;
    }

    let blocks = aligned_w * aligned_h / AFBC_PIXELS_PER_BLOCK;
    let header_bytes = align_up(blocks * AFBC_HEADER_BYTES_PER_BLOCK, header_align);
    let body_per_block = align_up(bpp * AFBC_PIXELS_PER_BLOCK / 8, AFBC_BODY_ALIGN);
    let total = header_bytes + blocks * body_per_block;
    let Ok(total) = u32::try_from(total) else {
        return -EINVAL;
    };
    out.afbc_size = total;
    0
}

/// Initialises an AFBC framebuffer's geometry and checks that the backing GEM
/// object can hold the compressed data.
///
/// Returns 0 on success, or a negative error code from the layout computation
/// or `-EINVAL` when the object is too small.
///
/// # Safety
/// `info` and `mode_cmd` must be valid. `afbc_fb` must be a writable AFBC
/// framebuffer whose `base.obj[0]` is a live GEM object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_fb_afbc_init(
    _dev: *mut DrmDevice,
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
    afbc_fb: *mut DrmAfbcFramebuffer,
) -> i32 {
    // SAFETY: `afbc_fb` is valid per the caller contract; `afbc_layout` runs first, so the size fields it writes are valid before they are read.
    unsafe {
        let ret = afbc_layout(info, mode_cmd, afbc_fb);
        if ret < 0 {
            return ret;
        }
        let obj = (*afbc_fb).base.obj[0];
        let required = (*afbc_fb).afbc_size as usize;
        if (*obj).size < required {
            debug_log(&alloc::format!(
                "GEM object size ({}) smaller than minimum afbc size ({required})\n",
                (*obj).size
            ));
            return -EINVAL;
        }
        0
    }
}
