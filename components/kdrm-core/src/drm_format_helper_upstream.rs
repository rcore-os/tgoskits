// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_format_helper.c` (v7.2.3) together with
//! the XRGB8888 pixel helpers of `drivers/gpu/drm/drm_format_internal.h`.
//!
//! Scope: the format-conversion state (`drm_format_conv_state_*`), the clip
//! byte offset, the `drm_fb_xfrm()` transfer engine, `drm_fb_memcpy()`, and
//! every `drm_fb_xrgb8888_to_*()` converter that `drm_sysfb_get_blit_func()`
//! (drm_sysfb_modeset.c) dispatches to. The swab, gray, mono, argb4444 and
//! argb8888->argb4444 converters are not reachable from the sysfb/efidrm path
//! and are not ported here.
//!
//! Deviations from the C source:
//! - The 32-bit line helpers address the source and destination through byte
//!   offsets with unaligned little-endian loads and stores (`read_unaligned`,
//!   `to_le`) instead of `__le32 *` casts. The values are identical on the
//!   little-endian x86_64 target and no alignment is assumed for scanout
//!   buffers. The pixel expressions are kept literally.
//! - `memcpy_toio()` is a plain copy; the MMIO barriers are not modelled.
//! - `ARCH_KMALLOC_MINALIGN` is 8 on x86_64 and is used as a constant.

use core::{ffi::c_void, ptr};

use drm_kapi::{
    config::{ENOMEM, GFP_KERNEL},
    memory::{kfree, krealloc},
};

use crate::{
    drm_format_helper_types::DrmFormatConvState,
    drm_fourcc_types::DrmFormatInfo,
    drm_fourcc_upstream::drm_format_info_bpp,
    drm_framebuffer_types::{DRM_FORMAT_MAX_PLANES, DrmFramebuffer},
    drm_gem_framebuffer_helper_upstream::iosys_map_incr,
    drm_rect_types::DrmRect,
    drm_rect_upstream::{drm_rect_height, drm_rect_width},
    iosys_map_types::IosysMap,
};

/// `ARCH_KMALLOC_MINALIGN` on x86_64 (`__alignof__(unsigned long long)`).
const ARCH_KMALLOC_MINALIGN: usize = 8;

/// Pixel line transformer function pointer: `void (*xfrm_line)(void *dbuf, const void *sbuf, unsigned int npixels)`.
///
/// # Safety
///
/// When invoked, `dbuf` must point to an unaliased destination buffer with space for at least
/// `npixels` destination pixels. `sbuf` must point to a readable source buffer with space for at
/// least `npixels` source pixels.
type XfrmLine = unsafe fn(*mut c_void, *const c_void, u32);

// ---------------------------------------------------------------------------
// Little-endian byte-offset access helpers (stand-ins for the __le casts).
// ---------------------------------------------------------------------------

/// Reads an unaligned little-endian 32-bit pixel value from a source buffer.
///
/// # Safety
///
/// `src` must point to readable memory extending at least `(pixel + 1) * 4` bytes.
#[inline(always)]
unsafe fn ld_le32_px(src: *const c_void, pixel: usize) -> u32 {
    // upstream: drm_format_helper.c le32_to_cpup(sbuf32 + pixel)
    // SAFETY: The memory range `[src + pixel * 4 .. src + (pixel + 1) * 4]` is within the bounds
    // of the source row buffer; `read_unaligned` safely reads 4 bytes regardless of pointer alignment.
    u32::from_le(unsafe { ptr::read_unaligned((src as *const u8).add(pixel * 4).cast::<u32>()) })
}

/// Stores an 8-bit pixel value to a destination buffer at the given byte offset.
///
/// # Safety
///
/// `dst` must point to writable memory extending at least `off + 1` bytes without aliasing.
#[inline(always)]
unsafe fn st_u8(dst: *mut c_void, off: usize, v: u8) {
    // SAFETY: Destination buffer allocation extends at least `off + 1` bytes; writing a single
    // byte requires no multi-byte alignment or endian reordering.
    unsafe { *(dst as *mut u8).add(off) = v };
}

/// Stores an unaligned little-endian 16-bit pixel value to a destination buffer.
///
/// # Safety
///
/// `dst` must point to writable memory extending at least `off + 2` bytes without aliasing.
#[inline(always)]
unsafe fn st_u16(dst: *mut c_void, off: usize, v: u16) {
    // SAFETY: Destination buffer allocation extends at least `off + 2` bytes; `write_unaligned`
    // writes the 2-byte little-endian value without requiring 16-bit pointer alignment.
    unsafe { ptr::write_unaligned((dst as *mut u8).add(off).cast::<u16>(), v.to_le()) };
}

/// Stores an unaligned little-endian 32-bit pixel value to a destination buffer.
///
/// # Safety
///
/// `dst` must point to writable memory extending at least `off + 4` bytes without aliasing.
#[inline(always)]
unsafe fn st_u32(dst: *mut c_void, off: usize, v: u32) {
    // SAFETY: Destination buffer allocation extends at least `off + 4` bytes; `write_unaligned`
    // writes the 4-byte little-endian value without requiring 32-bit pointer alignment.
    unsafe { ptr::write_unaligned((dst as *mut u8).add(off).cast::<u32>(), v.to_le()) };
}

/// Stores an unaligned little-endian 64-bit pixel value to a destination buffer.
///
/// # Safety
///
/// `dst` must point to writable memory extending at least `off + 8` bytes without aliasing.
#[inline(always)]
unsafe fn st_u64(dst: *mut c_void, off: usize, v: u64) {
    // SAFETY: Destination buffer allocation extends at least `off + 8` bytes; `write_unaligned`
    // writes the 8-byte little-endian value without requiring 64-bit pointer alignment.
    unsafe { ptr::write_unaligned((dst as *mut u8).add(off).cast::<u64>(), v.to_le()) };
}

// ---------------------------------------------------------------------------
// Format-conversion state
// ---------------------------------------------------------------------------

// upstream: drm_format_helper.c drm_format_conv_state_init()
#[unsafe(no_mangle)]
/// Initializes a format-conversion state structure.
///
/// # Safety
///
/// `state` must be non-null, properly aligned, and point to uninitialized or reusable
/// `DrmFormatConvState` storage with exclusive mutable access for the caller.
pub unsafe extern "C" fn drm_format_conv_state_init(state: *mut DrmFormatConvState) {
    // SAFETY: `state` is a non-null, aligned pointer to `DrmFormatConvState`; initializing
    // `tmp` fields to null/zero establishes an empty, non-preallocated temporary state.
    unsafe {
        (*state).tmp.mem = ptr::null_mut();
        (*state).tmp.size = 0;
        (*state).tmp.preallocated = false;
    }
}

// upstream: drm_format_helper.c drm_format_conv_state_copy()
#[unsafe(no_mangle)]
/// Copies format-conversion state parameters into a new state.
///
/// Temporary scratch memory is not duplicated; `state` is initialized to an empty temporary buffer.
///
/// # Safety
///
/// `state` must be non-null, properly aligned, and point to unaliased `DrmFormatConvState` memory.
/// `_old_state` is unused by this implementation and may be any pointer.
pub unsafe extern "C" fn drm_format_conv_state_copy(
    state: *mut DrmFormatConvState,
    _old_state: *const DrmFormatConvState,
) {
    // So far there is only temporary storage, which is not duplicated.
    // SAFETY: Temporary allocations are not shared between atomic states; zeroing `tmp`
    // in `state` sets up an empty allocation tracker without taking ownership of `_old_state`.
    unsafe {
        (*state).tmp.mem = ptr::null_mut();
        (*state).tmp.size = 0;
        (*state).tmp.preallocated = false;
    }
}

// upstream: drm_format_helper.c drm_format_conv_state_reserve()
#[unsafe(no_mangle)]
/// Reserves at least `new_size` bytes in the format-conversion state's temporary buffer.
///
/// Returns a pointer to the reserved memory buffer on success, or null on allocation failure
/// or if `state` uses preallocated fixed memory that cannot be expanded.
///
/// # Safety
///
/// `state` must be non-null, properly aligned, and point to an initialized `DrmFormatConvState`
/// that is not concurrently accessed by other threads.
pub unsafe extern "C" fn drm_format_conv_state_reserve(
    state: *mut DrmFormatConvState,
    new_size: usize,
    flags: u32,
) -> *mut c_void {
    // SAFETY: `state` is non-null and points to an initialized `DrmFormatConvState`; reading
    // `tmp.size` checks whether the current buffer is already sufficiently large.
    if new_size <= unsafe { (*state).tmp.size } {
        // SAFETY: `tmp.size >= new_size` confirms that `tmp.mem` holds at least `new_size` bytes.
        return unsafe { (*state).tmp.mem };
    // SAFETY: `state` is non-null and points to an initialized `DrmFormatConvState`; inspecting
    // `tmp.preallocated` to prevent reallocation of fixed caller-owned memory.
    } else if unsafe { (*state).tmp.preallocated } {
        return ptr::null_mut();
    }

    // SAFETY: `tmp.mem` is either null or a previous heap allocation from `krealloc`; resizing
    // to `new_size` bytes using the specified kernel GFP allocation flags.
    let mem = unsafe { krealloc((*state).tmp.mem, new_size, flags) };
    if mem.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `mem` is the non-null reallocated memory buffer with capacity of at least `new_size`
    // bytes; updating `tmp.mem` and `tmp.size` stores the new buffer in `state`.
    unsafe {
        (*state).tmp.mem = mem;
        (*state).tmp.size = new_size;
        (*state).tmp.mem
    }
}

// upstream: drm_format_helper.c drm_format_conv_state_release()
#[unsafe(no_mangle)]
/// Releases the temporary buffer held by the format-conversion state.
///
/// # Safety
///
/// `state` must be non-null, properly aligned, and point to an initialized `DrmFormatConvState`
/// that is not concurrently accessed by other threads.
pub unsafe extern "C" fn drm_format_conv_state_release(state: *mut DrmFormatConvState) {
    // SAFETY: `state` is non-null and points to an initialized `DrmFormatConvState`; checking
    // `tmp.preallocated` ensures externally managed preallocated memory is never freed.
    if unsafe { (*state).tmp.preallocated } {
        return;
    }
    // SAFETY: `tmp.mem` was allocated dynamically via `krealloc` (or is null, which `kfree`
    // safely ignores); clearing `tmp.mem` and `tmp.size` prevents dangling pointers or double frees.
    unsafe {
        kfree((*state).tmp.mem);
        (*state).tmp.mem = ptr::null_mut();
        (*state).tmp.size = 0;
    }
}

// ---------------------------------------------------------------------------
// Clip offsets
// ---------------------------------------------------------------------------

// upstream: drm_format_helper.c clip_offset()
#[inline]
fn clip_offset(clip: &DrmRect, pitch: u32, cpp: u32) -> u32 {
    (clip.y1 as u32)
        .wrapping_mul(pitch)
        .wrapping_add((clip.x1 as u32).wrapping_mul(cpp))
}

// upstream: drm_format_helper.c drm_fb_clip_offset()
#[unsafe(no_mangle)]
/// Computes the byte offset into a framebuffer plane for the top-left coordinate of a clip rectangle.
///
/// # Safety
///
/// `format` must be non-null and point to an initialized `DrmFormatInfo` with valid `cpp` entries.
/// `clip` must be non-null, aligned, and point to an initialized `DrmRect`.
pub unsafe extern "C" fn drm_fb_clip_offset(
    pitch: u32,
    format: *const DrmFormatInfo,
    clip: *const DrmRect,
) -> u32 {
    // SAFETY: `clip` and `format` are valid non-null pointers; dereferencing `(*format)._anon_6.cpp[0]`
    // retrieves bytes per pixel for plane 0 to compute `clip.y1 * pitch + clip.x1 * cpp`.
    unsafe { clip_offset(&*clip, pitch, (*format)._anon_6.cpp[0] as u32) }
}

// ---------------------------------------------------------------------------
// Transfer engine
// ---------------------------------------------------------------------------

// upstream: drm_format_helper.c __drm_fb_xfrm()
/// Line-by-line format conversion engine transferring pixels to a system-memory destination buffer.
///
/// # Safety
///
/// - `dst` must point to an unaliased writable system-memory buffer with space for `lines` rows of stride `dst_pitch`.
/// - `vaddr` must point to a readable system-memory scanout buffer with space for `lines` rows of stride `pitch0`.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with initialized `format` and `pitches[0]`.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` whose boundaries lie within `fb`.
/// - If `vaddr_cached_hint` is false, `state` must point to a valid `DrmFormatConvState` to reserve line scratch memory.
/// - `xfrm_line` must be a valid pixel transformation function matching the input and output formats.
// Keeps the Linux function signature.
struct LineDestination {
    dst: *mut c_void,
    dst_pitch: usize,
    dst_pixsize: usize,
}

struct MappedDestination {
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    dst_pixsize: *const u8,
}

unsafe fn __drm_fb_xfrm(
    destination: LineDestination,
    vaddr: *const c_void,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    vaddr_cached_hint: bool,
    state: *mut DrmFormatConvState,
    xfrm_line: XfrmLine,
) -> i32 {
    let LineDestination {
        dst,
        dst_pitch,
        dst_pixsize,
    } = destination;
    // SAFETY: `clip` is a valid, aligned pointer to `DrmRect`; borrowing as shared reference to read dimensions.
    let clip = unsafe { &*clip };
    let linepixels = drm_rect_width(clip) as usize;
    let lines = drm_rect_height(clip) as usize;
    // SAFETY: `fb` and `(*fb).format` are valid DRM pointers; reading `cpp[0]` yields bytes per source pixel.
    let cpp = unsafe { (*(*fb).format)._anon_6.cpp[0] as usize };
    let sbuf_len = linepixels * cpp;
    // SAFETY: `fb` is a valid `DrmFramebuffer`; reading `pitches[0]` yields the plane 0 stride in bytes.
    let pitch0 = unsafe { (*fb).pitches[0] as usize };

    // Some source buffers (DMA memory, write-combined) are slow to read, so
    // fetch one line at a time into a temporary.
    let mut stmp: *mut c_void = ptr::null_mut();
    if !vaddr_cached_hint {
        // SAFETY: `state` is a valid pointer to `DrmFormatConvState`; reserving `sbuf_len` bytes
        // to stage uncached scanline reads into cached system memory.
        stmp = unsafe { drm_format_conv_state_reserve(state, sbuf_len, GFP_KERNEL) };
        if stmp.is_null() {
            return -ENOMEM;
        }
    }

    let dst_pitch = if dst_pitch == 0 {
        drm_rect_width(clip) as usize * dst_pixsize
    } else {
        dst_pitch
    };

    // SAFETY: `clip_offset` computes `clip.y1 * pitch0 + clip.x1 * cpp`, which is within the source
    // framebuffer boundaries; adding this byte offset locates the top-left coordinate of the clip rect.
    let mut vaddr = unsafe {
        vaddr
            .cast::<u8>()
            .add(clip_offset(clip, pitch0 as u32, cpp as u32) as usize)
    };
    let mut dst = dst.cast::<u8>();
    for _ in 0..lines {
        let sbuf: *const c_void = if !stmp.is_null() {
            // SAFETY: `vaddr` provides `sbuf_len` readable bytes for the current line; `stmp` is a
            // non-overlapping temporary buffer of at least `sbuf_len` allocated bytes.
            unsafe { ptr::copy_nonoverlapping(vaddr, stmp.cast::<u8>(), sbuf_len) };
            stmp
        } else {
            vaddr.cast()
        };
        // SAFETY: `sbuf` points to `linepixels` valid source pixels; `dst` points to an unaliased
        // row buffer with capacity for at least `linepixels * dst_pixsize` bytes.
        unsafe { xfrm_line(dst.cast(), sbuf, linepixels as u32) };
        // SAFETY: Advancing `vaddr` by `pitch0` stays within the allocated source buffer for `lines` iterations.
        vaddr = unsafe { vaddr.add(pitch0) };
        // SAFETY: Advancing `dst` by `dst_pitch` stays within the destination buffer for `lines` iterations.
        dst = unsafe { dst.add(dst_pitch) };
    }
    0
}

// upstream: drm_format_helper.c __drm_fb_xfrm_toio()
/// Line-by-line format conversion engine transferring pixels to a memory-mapped I/O destination buffer.
///
/// # Safety
///
/// - `dst` must point to an MMIO destination buffer (I/O memory) with space for `lines` rows of stride `dst_pitch`.
/// - `vaddr` must point to a readable system-memory scanout buffer with space for `lines` rows of stride `pitch0`.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with initialized `format` and `pitches[0]`.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` whose boundaries lie within `fb`.
/// - `state` must point to a valid `DrmFormatConvState` to reserve bounce buffer memory.
/// - `xfrm_line` must be a valid pixel transformation function matching the input and output formats.
// Keeps the Linux function signature.
unsafe fn __drm_fb_xfrm_toio(
    destination: LineDestination,
    vaddr: *const c_void,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    vaddr_cached_hint: bool,
    state: *mut DrmFormatConvState,
    xfrm_line: XfrmLine,
) -> i32 {
    let LineDestination {
        dst,
        dst_pitch,
        dst_pixsize,
    } = destination;
    // SAFETY: `clip` is a valid, aligned pointer to `DrmRect`; borrowing as shared reference to read dimensions.
    let clip = unsafe { &*clip };
    let linepixels = drm_rect_width(clip) as usize;
    let lines = drm_rect_height(clip) as usize;
    let dbuf_len = linepixels * dst_pixsize;
    let stmp_off = (dbuf_len + ARCH_KMALLOC_MINALIGN - 1) & !(ARCH_KMALLOC_MINALIGN - 1);
    // SAFETY: `fb` and `(*fb).format` are valid DRM pointers; reading `cpp[0]` yields bytes per source pixel.
    let cpp = unsafe { (*(*fb).format)._anon_6.cpp[0] as usize };
    let sbuf_len = linepixels * cpp;
    // SAFETY: `fb` is a valid `DrmFramebuffer`; reading `pitches[0]` yields the plane 0 stride in bytes.
    let pitch0 = unsafe { (*fb).pitches[0] as usize };

    let mut stmp: *mut u8 = ptr::null_mut();
    let dbuf: *mut c_void = if vaddr_cached_hint {
        // SAFETY: `state` is a valid `DrmFormatConvState`; reserving bounce buffer `dbuf_len` for transformed row.
        unsafe { drm_format_conv_state_reserve(state, dbuf_len, GFP_KERNEL) }
    } else {
        // SAFETY: `state` is a valid `DrmFormatConvState`; reserving contiguous bounce buffer `stmp_off + sbuf_len`.
        unsafe { drm_format_conv_state_reserve(state, stmp_off + sbuf_len, GFP_KERNEL) }
    };
    if dbuf.is_null() {
        return -ENOMEM;
    }
    if !vaddr_cached_hint {
        // SAFETY: `dbuf` was reserved with capacity `stmp_off + sbuf_len`, and `stmp_off` is aligned to
        // `ARCH_KMALLOC_MINALIGN`; adding `stmp_off` yields a valid pointer within the reserved allocation.
        stmp = unsafe { dbuf.cast::<u8>().add(stmp_off) };
    }

    let dst_pitch = if dst_pitch == 0 {
        linepixels * dst_pixsize
    } else {
        dst_pitch
    };

    // SAFETY: `clip_offset` computes `clip.y1 * pitch0 + clip.x1 * cpp`, which is within the source
    // framebuffer boundaries; adding this byte offset locates the top-left coordinate of the clip rect.
    let mut vaddr = unsafe {
        vaddr
            .cast::<u8>()
            .add(clip_offset(clip, pitch0 as u32, cpp as u32) as usize)
    };
    let mut dst = dst.cast::<u8>();
    for _ in 0..lines {
        let sbuf: *const c_void = if !stmp.is_null() {
            // SAFETY: `vaddr` provides `sbuf_len` readable bytes for the current line; `stmp` is a
            // non-overlapping temporary bounce buffer of at least `sbuf_len` allocated bytes.
            unsafe { ptr::copy_nonoverlapping(vaddr, stmp, sbuf_len) };
            stmp.cast()
        } else {
            vaddr.cast()
        };
        // SAFETY: `sbuf` points to `linepixels` valid source pixels; `dbuf` points to the system bounce
        // buffer with capacity for `dbuf_len` (`linepixels * dst_pixsize`) bytes.
        unsafe { xfrm_line(dbuf, sbuf, linepixels as u32) };
        // memcpy_toio(dst, dbuf, dbuf_len)
        // SAFETY: `dbuf` contains `dbuf_len` transformed bytes in system memory; `dst` points to an MMIO
        // destination row with capacity for at least `dbuf_len` bytes.
        unsafe { ptr::copy_nonoverlapping(dbuf.cast::<u8>(), dst, dbuf_len) };
        // SAFETY: Advancing `vaddr` by `pitch0` stays within the allocated source buffer for `lines` iterations.
        vaddr = unsafe { vaddr.add(pitch0) };
        // SAFETY: Advancing `dst` by `dst_pitch` stays within the MMIO destination buffer for `lines` iterations.
        dst = unsafe { dst.add(dst_pitch) };
    }
    0
}

// upstream: drm_format_helper.c drm_fb_xfrm()
/// Dispatches framebuffer format conversion to system memory or MMIO based on the `dst` mapping type.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` describing the destination buffer.
/// - `dst_pitch` may be null (indicating tightly packed rows) or point to an aligned `u32` row stride in bytes.
/// - `dst_pixsize` must point to an initialized `u8` specifying bytes per destination pixel.
/// - `src` must point to an initialized `IosysMap` describing system-memory scanout buffer.
/// - `fb` must point to a valid `DrmFramebuffer` describing the source framebuffer.
/// - `clip` must point to a valid `DrmRect` bounded by framebuffer dimensions.
/// - `state` must point to an initialized `DrmFormatConvState`.
/// - `xfrm_line` must be a valid row-transformation function pointer.
// Keeps the Linux function signature.
unsafe fn drm_fb_xfrm(
    destination: MappedDestination,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    vaddr_cached_hint: bool,
    state: *mut DrmFormatConvState,
    xfrm_line: XfrmLine,
) -> i32 {
    let MappedDestination {
        dst,
        dst_pitch,
        dst_pixsize,
    } = destination;
    // default_dst_pitch[] is all zero: a NULL pitch means "tight packing".
    let pitch0 = if dst_pitch.is_null() {
        0
    } else {
        // SAFETY: `dst_pitch` is verified non-null in the `if` guard and points to an aligned `u32` stride value.
        unsafe { *dst_pitch as usize }
    };
    // SAFETY: `dst_pixsize` is a non-null pointer to an initialized `u8` indicating bytes per destination pixel.
    let pixsize0 = unsafe { *dst_pixsize as usize };

    // TODO (upstream): handle src in I/O memory here.
    // SAFETY: `dst` is non-null and points to an initialized `IosysMap`; checking `is_iomem` to branch.
    if unsafe { (*dst).is_iomem } {
        // SAFETY: `dst` is an MMIO mapping; `vaddr_iomem` points to destination I/O memory, while `src` provides
        // system-memory scanout data; `fb`, `clip`, and `state` are valid pointers.
        unsafe {
            __drm_fb_xfrm_toio(
                LineDestination {
                    dst: (*dst)._anon_0.vaddr_iomem,
                    dst_pitch: pitch0,
                    dst_pixsize: pixsize0,
                },
                (*src)._anon_0.vaddr,
                fb,
                clip,
                vaddr_cached_hint,
                state,
                xfrm_line,
            )
        }
    } else {
        // SAFETY: `dst` is a system-memory mapping; `vaddr` points to destination RAM, while `src` provides
        // system-memory scanout data; `fb`, `clip`, and `state` are valid pointers.
        unsafe {
            __drm_fb_xfrm(
                LineDestination {
                    dst: (*dst)._anon_0.vaddr,
                    dst_pitch: pitch0,
                    dst_pixsize: pixsize0,
                },
                (*src)._anon_0.vaddr,
                fb,
                clip,
                vaddr_cached_hint,
                state,
                xfrm_line,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// 32-bit source line helpers (drm_format_helper.c drm_fb_xfrm_line_32toN)
// ---------------------------------------------------------------------------

// upstream: drm_format_helper.c drm_fb_xfrm_line_32to8()
/// Converts a line of 32-bit source pixels to 8-bit destination pixels.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of little-endian 32-bit pixels.
/// `dbuf` must point to at least `pixels` unaliased writable bytes for the 8-bit output.
#[inline(always)]
unsafe fn drm_fb_xfrm_line_32to8(
    dbuf: *mut c_void,
    sbuf: *const c_void,
    pixels: u32,
    xfrm_pixel: fn(u32) -> u32,
) {
    let pixels = pixels as usize;
    let mut i = 0usize;
    // write 4 pixels at once; each pixel is one output byte
    while i + 4 <= pixels {
        let pix = [
            // SAFETY: `sbuf` has `pixels * 4` readable bytes; pixel index `i < i + 4 <= pixels` is within range.
            unsafe { ld_le32_px(sbuf, i) },
            // SAFETY: Pixel index `i + 1 < i + 4 <= pixels` is within range.
            unsafe { ld_le32_px(sbuf, i + 1) },
            // SAFETY: Pixel index `i + 2 < i + 4 <= pixels` is within range.
            unsafe { ld_le32_px(sbuf, i + 2) },
            // SAFETY: Pixel index `i + 3 < i + 4 <= pixels` is within range.
            unsafe { ld_le32_px(sbuf, i + 3) },
        ];
        let val32 = xfrm_pixel(pix[0])
            | (xfrm_pixel(pix[1]) << 8)
            | (xfrm_pixel(pix[2]) << 16)
            | (xfrm_pixel(pix[3]) << 24);
        // SAFETY: `dbuf` has `pixels` writable bytes; byte offset `i .. i + 4 <= pixels` is within range.
        unsafe { st_u32(dbuf, i, val32) };
        i += 4;
    }
    // trailing pixels
    while i < pixels {
        // SAFETY: `sbuf` has `(i + 1) * 4` readable bytes and `dbuf` has `i + 1` writable bytes for `i < pixels`.
        unsafe { st_u8(dbuf, i, xfrm_pixel(ld_le32_px(sbuf, i)) as u8) };
        i += 1;
    }
}

// upstream: drm_format_helper.c drm_fb_xfrm_line_32to16()  (CONFIG_64BIT path)
/// Converts a line of 32-bit source pixels to 16-bit destination pixels.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of little-endian 32-bit pixels.
/// `dbuf` must point to at least `pixels * 2` unaliased writable bytes for the 16-bit output.
#[inline(always)]
unsafe fn drm_fb_xfrm_line_32to16(
    dbuf: *mut c_void,
    sbuf: *const c_void,
    pixels: u32,
    xfrm_pixel: fn(u32) -> u32,
) {
    let pixels = pixels as usize;
    let mut i = 0usize;
    // write 4 pixels at once
    while i + 4 <= pixels {
        let pix = [
            // SAFETY: `sbuf` has `pixels * 4` readable bytes; pixel index `i < i + 4 <= pixels` is within range.
            unsafe { ld_le32_px(sbuf, i) },
            // SAFETY: Pixel index `i + 1 < i + 4 <= pixels` is within range.
            unsafe { ld_le32_px(sbuf, i + 1) },
            // SAFETY: Pixel index `i + 2 < i + 4 <= pixels` is within range.
            unsafe { ld_le32_px(sbuf, i + 2) },
            // SAFETY: Pixel index `i + 3 < i + 4 <= pixels` is within range.
            unsafe { ld_le32_px(sbuf, i + 3) },
        ];
        let val64 = (xfrm_pixel(pix[0]) as u64)
            | ((xfrm_pixel(pix[1]) as u64) << 16)
            | ((xfrm_pixel(pix[2]) as u64) << 32)
            | ((xfrm_pixel(pix[3]) as u64) << 48);
        // SAFETY: `dbuf` has `pixels * 2` writable bytes; byte offset `i * 2 .. i * 2 + 8 <= pixels * 2`.
        unsafe { st_u64(dbuf, i * 2, val64) };
        i += 4;
    }
    // write 2 pixels at once
    while i + 2 <= pixels {
        // SAFETY: `i + 1 < pixels` follows from the loop guard `i + 2 <= pixels`, so both `ld_le32_px` reads stay within the `sbuf` pixels.
        let pix = [unsafe { ld_le32_px(sbuf, i) }, unsafe {
            ld_le32_px(sbuf, i + 1)
        }];
        let val32 = xfrm_pixel(pix[0]) | (xfrm_pixel(pix[1]) << 16);
        // SAFETY: `dbuf` has `pixels * 2` writable bytes; byte offset `i * 2 .. i * 2 + 4 <= pixels * 2`.
        unsafe { st_u32(dbuf, i * 2, val32) };
        i += 2;
    }
    // trailing pixel
    while i < pixels {
        // SAFETY: `sbuf` has `(i + 1) * 4` readable bytes and `dbuf` has `(i + 1) * 2` writable bytes for `i < pixels`.
        unsafe { st_u16(dbuf, i * 2, xfrm_pixel(ld_le32_px(sbuf, i)) as u16) };
        i += 1;
    }
}

// upstream: drm_format_helper.c drm_fb_xfrm_line_32to24()
/// Converts a line of 32-bit source pixels to 24-bit destination pixels.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of little-endian 32-bit pixels.
/// `dbuf` must point to at least `pixels * 3` unaliased writable bytes for the 24-bit output.
#[inline(always)]
unsafe fn drm_fb_xfrm_line_32to24(
    dbuf: *mut c_void,
    sbuf: *const c_void,
    pixels: u32,
    xfrm_pixel: fn(u32) -> u32,
) {
    let pixels = pixels as usize;
    let mut i = 0usize;
    // write pixels in chunks of 4 (12 output bytes)
    while i + 4 <= pixels {
        let val24 = [
            // SAFETY: `sbuf` has `pixels * 4` readable bytes; pixel index `i < i + 4 <= pixels` is within range.
            xfrm_pixel(unsafe { ld_le32_px(sbuf, i) }),
            // SAFETY: Pixel index `i + 1 < i + 4 <= pixels` is within range.
            xfrm_pixel(unsafe { ld_le32_px(sbuf, i + 1) }),
            // SAFETY: Pixel index `i + 2 < i + 4 <= pixels` is within range.
            xfrm_pixel(unsafe { ld_le32_px(sbuf, i + 2) }),
            // SAFETY: Pixel index `i + 3 < i + 4 <= pixels` is within range.
            xfrm_pixel(unsafe { ld_le32_px(sbuf, i + 3) }),
        ];
        let out32 = [
            (val24[0] & 0x000000ff)
                | (val24[0] & 0x0000ff00)
                | (val24[0] & 0x00ff0000)
                | ((val24[1] & 0x000000ff) << 24),
            ((val24[1] & 0x0000ff00) >> 8)
                | ((val24[1] & 0x00ff0000) >> 8)
                | ((val24[2] & 0x000000ff) << 16)
                | ((val24[2] & 0x0000ff00) << 16),
            ((val24[2] & 0x00ff0000) >> 16)
                | ((val24[3] & 0x000000ff) << 8)
                | ((val24[3] & 0x0000ff00) << 8)
                | ((val24[3] & 0x00ff0000) << 8),
        ];
        // SAFETY: `dbuf` has `pixels * 3` writable bytes; offset `i * 3 .. i * 3 + 4 <= pixels * 3`.
        unsafe { st_u32(dbuf, i * 3, out32[0]) };
        // SAFETY: Offset `i * 3 + 4 .. i * 3 + 8 <= pixels * 3`.
        unsafe { st_u32(dbuf, i * 3 + 4, out32[1]) };
        // SAFETY: Offset `i * 3 + 8 .. i * 3 + 12 <= pixels * 3`.
        unsafe { st_u32(dbuf, i * 3 + 8, out32[2]) };
        i += 4;
    }
    // trailing pixels, three bytes each, little-endian
    while i < pixels {
        // SAFETY: `sbuf` has `(i + 1) * 4` readable bytes for index `i < pixels`.
        let val24 = xfrm_pixel(unsafe { ld_le32_px(sbuf, i) });
        // SAFETY: `dbuf` has `pixels * 3` writable bytes; writing byte 0 at `i * 3 < pixels * 3`.
        unsafe { st_u8(dbuf, i * 3, (val24 & 0x000000ff) as u8) };
        // SAFETY: Writing byte 1 at `i * 3 + 1 < pixels * 3`.
        unsafe { st_u8(dbuf, i * 3 + 1, ((val24 & 0x0000ff00) >> 8) as u8) };
        // SAFETY: Writing byte 2 at `i * 3 + 2 < pixels * 3`.
        unsafe { st_u8(dbuf, i * 3 + 2, ((val24 & 0x00ff0000) >> 16) as u8) };
        i += 1;
    }
}

// upstream: drm_format_helper.c drm_fb_xfrm_line_32to32()
/// Converts a line of 32-bit source pixels to 32-bit destination pixels.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of little-endian 32-bit pixels.
/// `dbuf` must point to at least `pixels * 4` unaliased writable bytes for the 32-bit output.
#[inline(always)]
unsafe fn drm_fb_xfrm_line_32to32(
    dbuf: *mut c_void,
    sbuf: *const c_void,
    pixels: u32,
    xfrm_pixel: fn(u32) -> u32,
) {
    for i in 0..pixels as usize {
        // SAFETY: Loop index `i < pixels`: `ld_le32_px` reads 4 bytes from `sbuf + i * 4`, and `st_u32`
        // writes 4 bytes to `dbuf + i * 4`, both within their `pixels * 4` allocations.
        unsafe { st_u32(dbuf, i * 4, xfrm_pixel(ld_le32_px(sbuf, i))) };
    }
}

// ---------------------------------------------------------------------------
// drm_format_internal.h pixel helpers (XRGB8888 source)
// ---------------------------------------------------------------------------

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_rgb332()
#[inline]
fn drm_pixel_xrgb8888_to_rgb332(pix: u32) -> u32 {
    ((pix & 0x00e00000) >> 16) | ((pix & 0x0000e000) >> 11) | ((pix & 0x000000c0) >> 6)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_rgb565()
#[inline]
fn drm_pixel_xrgb8888_to_rgb565(pix: u32) -> u32 {
    ((pix & 0x00f80000) >> 8) | ((pix & 0x0000fc00) >> 5) | ((pix & 0x000000f8) >> 3)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_rgb565be()
#[inline]
fn drm_pixel_xrgb8888_to_rgb565be(pix: u32) -> u32 {
    // swab16() on the 16-bit RGB565 value
    (drm_pixel_xrgb8888_to_rgb565(pix) as u16).swap_bytes() as u32
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_rgbx5551()
#[inline]
fn drm_pixel_xrgb8888_to_rgbx5551(pix: u32) -> u32 {
    ((pix & 0x00f80000) >> 8) | ((pix & 0x0000f800) >> 5) | ((pix & 0x000000f8) >> 2)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_rgba5551()
#[inline]
fn drm_pixel_xrgb8888_to_rgba5551(pix: u32) -> u32 {
    drm_pixel_xrgb8888_to_rgbx5551(pix) | (1 << 0) // set alpha bit
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_xrgb1555()
#[inline]
fn drm_pixel_xrgb8888_to_xrgb1555(pix: u32) -> u32 {
    ((pix & 0x00f80000) >> 9) | ((pix & 0x0000f800) >> 6) | ((pix & 0x000000f8) >> 3)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_argb1555()
#[inline]
fn drm_pixel_xrgb8888_to_argb1555(pix: u32) -> u32 {
    (1 << 15) | drm_pixel_xrgb8888_to_xrgb1555(pix) // set alpha bit
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_rgb888()
#[inline]
fn drm_pixel_xrgb8888_to_rgb888(pix: u32) -> u32 {
    pix & 0x00ff_ffff // GENMASK(23, 0)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_bgr888()
#[inline]
fn drm_pixel_xrgb8888_to_bgr888(pix: u32) -> u32 {
    ((pix & 0x00ff0000) >> 16) | (pix & 0x0000ff00) | ((pix & 0x000000ff) << 16)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_argb8888()
#[inline]
fn drm_pixel_xrgb8888_to_argb8888(pix: u32) -> u32 {
    0xff00_0000 | pix // GENMASK(31, 24): fill alpha bits
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_xbgr8888()
#[inline]
fn drm_pixel_xrgb8888_to_xbgr8888(pix: u32) -> u32 {
    (pix & 0xff00_0000)
        | ((pix & 0x00ff0000) >> 16)
        | (pix & 0x0000ff00)
        | ((pix & 0x000000ff) << 16)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_abgr8888()
#[inline]
fn drm_pixel_xrgb8888_to_abgr8888(pix: u32) -> u32 {
    0xff00_0000 | drm_pixel_xrgb8888_to_xbgr8888(pix)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_bgrx8888()
#[inline]
fn drm_pixel_xrgb8888_to_bgrx8888(pix: u32) -> u32 {
    ((pix & 0xff000000) >> 24)
        | ((pix & 0x00ff0000) >> 8)
        | ((pix & 0x0000ff00) << 8)
        | ((pix & 0x000000ff) << 24)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_xrgb2101010()
#[inline]
fn drm_pixel_xrgb8888_to_xrgb2101010(pix: u32) -> u32 {
    let pix = ((pix & 0x000000ff) << 2) | ((pix & 0x0000ff00) << 4) | ((pix & 0x00ff0000) << 6);
    pix | ((pix >> 8) & 0x00300c03)
}

// upstream: drm_format_internal.h drm_pixel_xrgb8888_to_argb2101010()
#[inline]
fn drm_pixel_xrgb8888_to_argb2101010(pix: u32) -> u32 {
    0xc000_0000 | drm_pixel_xrgb8888_to_xrgb2101010(pix) // GENMASK(31, 30)
}

// ---------------------------------------------------------------------------
// Line converters and exported XRGB8888 -> format entry points
// ---------------------------------------------------------------------------

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgb332_line()
/// Converts a single scanline of XRGB8888 pixels into RGB332.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels` unaliased writable bytes for RGB332 pixels.
unsafe fn drm_fb_xrgb8888_to_rgb332_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels` writable
    // bytes; `drm_pixel_xrgb8888_to_rgb332` maps each 32-bit pixel to an 8-bit output pixel.
    unsafe { drm_fb_xfrm_line_32to8(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_rgb332) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgb332()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to RGB332.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_rgb332(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [1, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 1 byte per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_rgb332_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_rgb332_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgb565_line()
/// Converts a single scanline of XRGB8888 pixels into RGB565.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 2` unaliased writable bytes for RGB565 pixels.
unsafe fn drm_fb_xrgb8888_to_rgb565_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 2` writable
    // bytes; `drm_pixel_xrgb8888_to_rgb565` maps each 32-bit pixel to a 16-bit output pixel.
    unsafe { drm_fb_xfrm_line_32to16(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_rgb565) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgb565()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to RGB565.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_rgb565(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [2, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 2 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_rgb565_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_rgb565_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgb565be_line()
/// Converts a single scanline of XRGB8888 pixels into big-endian RGB565.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 2` unaliased writable bytes for RGB565BE pixels.
unsafe fn drm_fb_xrgb8888_to_rgb565be_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 2` writable
    // bytes; `drm_pixel_xrgb8888_to_rgb565be` maps each 32-bit pixel to a big-endian 16-bit output pixel.
    unsafe { drm_fb_xfrm_line_32to16(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_rgb565be) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgb565be()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to big-endian RGB565.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_rgb565be(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [2, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 2 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_rgb565be_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_rgb565be_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_xrgb1555_line()
/// Converts a single scanline of XRGB8888 pixels into XRGB1555.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 2` unaliased writable bytes for XRGB1555 pixels.
unsafe fn drm_fb_xrgb8888_to_xrgb1555_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 2` writable
    // bytes; `drm_pixel_xrgb8888_to_xrgb1555` maps each 32-bit pixel to a 16-bit output pixel.
    unsafe { drm_fb_xfrm_line_32to16(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_xrgb1555) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_xrgb1555()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to XRGB1555.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_xrgb1555(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [2, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 2 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_xrgb1555_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_xrgb1555_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_argb1555_line()
/// Converts a single scanline of XRGB8888 pixels into ARGB1555.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 2` unaliased writable bytes for ARGB1555 pixels.
unsafe fn drm_fb_xrgb8888_to_argb1555_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 2` writable
    // bytes; `drm_pixel_xrgb8888_to_argb1555` maps each 32-bit pixel to a 16-bit output pixel with alpha set.
    unsafe { drm_fb_xfrm_line_32to16(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_argb1555) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_argb1555()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to ARGB1555.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_argb1555(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [2, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 2 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_argb1555_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_argb1555_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgba5551_line()
/// Converts a single scanline of XRGB8888 pixels into RGBA5551.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 2` unaliased writable bytes for RGBA5551 pixels.
unsafe fn drm_fb_xrgb8888_to_rgba5551_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 2` writable
    // bytes; `drm_pixel_xrgb8888_to_rgba5551` maps each 32-bit pixel to a 16-bit output pixel with alpha set.
    unsafe { drm_fb_xfrm_line_32to16(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_rgba5551) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgba5551()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to RGBA5551.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_rgba5551(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [2, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 2 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_rgba5551_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_rgba5551_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgb888_line()
/// Converts a single scanline of XRGB8888 pixels into RGB888.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 3` unaliased writable bytes for RGB888 pixels.
unsafe fn drm_fb_xrgb8888_to_rgb888_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 3` writable
    // bytes; `drm_pixel_xrgb8888_to_rgb888` maps each 32-bit pixel to a 24-bit RGB pixel.
    unsafe { drm_fb_xfrm_line_32to24(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_rgb888) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_rgb888()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to RGB888.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_rgb888(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [3, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 3 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_rgb888_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_rgb888_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_bgr888_line()
/// Converts a single scanline of XRGB8888 pixels into BGR888.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 3` unaliased writable bytes for BGR888 pixels.
unsafe fn drm_fb_xrgb8888_to_bgr888_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 3` writable
    // bytes; `drm_pixel_xrgb8888_to_bgr888` maps each 32-bit pixel to a 24-bit BGR pixel.
    unsafe { drm_fb_xfrm_line_32to24(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_bgr888) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_bgr888()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to BGR888.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_bgr888(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [3, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 3 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_bgr888_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_bgr888_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_argb8888_line()
/// Converts a single scanline of XRGB8888 pixels into ARGB8888.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 4` unaliased writable bytes for ARGB8888 pixels.
unsafe fn drm_fb_xrgb8888_to_argb8888_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 4` writable
    // bytes; `drm_pixel_xrgb8888_to_argb8888` maps each 32-bit pixel to a 32-bit pixel with alpha set to 0xff.
    unsafe { drm_fb_xfrm_line_32to32(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_argb8888) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_argb8888()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to ARGB8888.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_argb8888(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [4, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 4 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_argb8888_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_argb8888_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_abgr8888_line()
/// Converts a single scanline of XRGB8888 pixels into ABGR8888.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 4` unaliased writable bytes for ABGR8888 pixels.
unsafe fn drm_fb_xrgb8888_to_abgr8888_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 4` writable
    // bytes; `drm_pixel_xrgb8888_to_abgr8888` maps each 32-bit pixel to a 32-bit ABGR pixel with alpha set to 0xff.
    unsafe { drm_fb_xfrm_line_32to32(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_abgr8888) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_abgr8888()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to ABGR8888.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_abgr8888(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [4, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 4 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_abgr8888_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_abgr8888_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_xbgr8888_line()
/// Converts a single scanline of XRGB8888 pixels into XBGR8888.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 4` unaliased writable bytes for XBGR8888 pixels.
unsafe fn drm_fb_xrgb8888_to_xbgr8888_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 4` writable
    // bytes; `drm_pixel_xrgb8888_to_xbgr8888` maps each 32-bit pixel to a 32-bit XBGR pixel.
    unsafe { drm_fb_xfrm_line_32to32(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_xbgr8888) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_xbgr8888()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to XBGR8888.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_xbgr8888(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [4, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 4 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_xbgr8888_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_xbgr8888_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_bgrx8888_line()
/// Converts a single scanline of XRGB8888 pixels into BGRX8888.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 4` unaliased writable bytes for BGRX8888 pixels.
unsafe fn drm_fb_xrgb8888_to_bgrx8888_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 4` writable
    // bytes; `drm_pixel_xrgb8888_to_bgrx8888` maps each 32-bit pixel to a 32-bit BGRX pixel.
    unsafe { drm_fb_xfrm_line_32to32(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_bgrx8888) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_bgrx8888()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to BGRX8888.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_bgrx8888(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [4, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 4 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_bgrx8888_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_bgrx8888_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_xrgb2101010_line()
/// Converts a single scanline of XRGB8888 pixels into XRGB2101010.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 4` unaliased writable bytes for XRGB2101010 pixels.
unsafe fn drm_fb_xrgb8888_to_xrgb2101010_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 4` writable
    // bytes; `drm_pixel_xrgb8888_to_xrgb2101010` maps each 32-bit pixel to a 10-bit-per-channel format.
    unsafe { drm_fb_xfrm_line_32to32(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_xrgb2101010) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_xrgb2101010()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to XRGB2101010.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_xrgb2101010(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [4, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 4 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_xrgb2101010_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_xrgb2101010_line,
        )
    };
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_argb2101010_line()
/// Converts a single scanline of XRGB8888 pixels into ARGB2101010.
///
/// # Safety
///
/// `sbuf` must point to at least `pixels * 4` readable bytes of XRGB8888 pixels.
/// `dbuf` must point to at least `pixels * 4` unaliased writable bytes for ARGB2101010 pixels.
unsafe fn drm_fb_xrgb8888_to_argb2101010_line(dbuf: *mut c_void, sbuf: *const c_void, pixels: u32) {
    // SAFETY: `sbuf` provides `pixels * 4` readable bytes and `dbuf` provides `pixels * 4` writable
    // bytes; `drm_pixel_xrgb8888_to_argb2101010` maps each 32-bit pixel to a 10-bit format with top alpha bits set.
    unsafe { drm_fb_xfrm_line_32to32(dbuf, sbuf, pixels, drm_pixel_xrgb8888_to_argb2101010) }
}

// upstream: drm_format_helper.c drm_fb_xrgb8888_to_argb2101010()
#[unsafe(no_mangle)]
/// Converts a rectangular clip region of an XRGB8888 framebuffer to ARGB2101010.
///
/// # Safety
///
/// - `dst` must point to an initialized `IosysMap` mapping destination memory for the clip region.
/// - `dst_pitch` may be null for tightly packed rows or point to an aligned `u32` row stride in bytes.
/// - `src` must point to an initialized `IosysMap` mapping system-memory scanout buffer.
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with format XRGB8888.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
/// - `state` must point to an initialized `DrmFormatConvState`.
pub unsafe extern "C" fn drm_fb_xrgb8888_to_argb2101010(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
    state: *mut DrmFormatConvState,
) {
    static DST_PIXSIZE: [u8; DRM_FORMAT_MAX_PLANES] = [4, 0, 0, 0];
    // SAFETY: `DST_PIXSIZE` specifies 4 bytes per pixel for plane 0; `dst`, `src`, `fb`, `clip`,
    // and `state` are valid pointers forwarded to `drm_fb_xfrm` with `drm_fb_xrgb8888_to_argb2101010_line`.
    unsafe {
        drm_fb_xfrm(
            MappedDestination {
                dst,
                dst_pitch,
                dst_pixsize: DST_PIXSIZE.as_ptr(),
            },
            src,
            fb,
            clip,
            false,
            state,
            drm_fb_xrgb8888_to_argb2101010_line,
        )
    };
}

// ---------------------------------------------------------------------------
// drm_fb_memcpy()
// ---------------------------------------------------------------------------

// upstream: include/linux/iosys-map.h iosys_map_memcpy_to()
/// Copies memory into an `IosysMap` destination at the specified byte offset.
///
/// # Safety
///
/// `dst` must point to an initialized `IosysMap` descriptor with at least `dst_offset + len`
/// accessible bytes. `src` must point to readable memory of at least `len` bytes.
unsafe fn iosys_map_memcpy_to(
    dst: *mut IosysMap,
    dst_offset: usize,
    src: *const c_void,
    len: usize,
) {
    // SAFETY: `dst` is non-null and points to an initialized `IosysMap`; inspecting `is_iomem` tag.
    let base = if unsafe { (*dst).is_iomem } {
        // SAFETY: `dst` is marked MMIO; reading the `vaddr_iomem` I/O pointer.
        unsafe { (*dst)._anon_0.vaddr_iomem }
    } else {
        // SAFETY: `dst` is marked system memory; reading the `vaddr` virtual pointer.
        unsafe { (*dst)._anon_0.vaddr }
    };
    // SAFETY: `src` provides `len` readable bytes; `base.add(dst_offset)` provides `len` writable
    // bytes in the target mapping, and buffers do not overlap.
    unsafe { ptr::copy_nonoverlapping(src.cast::<u8>(), base.cast::<u8>().add(dst_offset), len) };
}

// upstream: drm_format_helper.c drm_fb_memcpy()
#[unsafe(no_mangle)]
/// Copies a rectangular clip region from a source framebuffer to a destination buffer plane by plane.
///
/// # Safety
///
/// - `dst` must point to an array of initialized `IosysMap` with at least `(*fb).format.num_planes` elements.
/// - `dst_pitch` may be null (tight packing) or point to an array of `u32` pitches with at least `num_planes` elements.
/// - `src` must point to an array of initialized `IosysMap` with at least `num_planes` elements (system-memory mappings).
/// - `fb` must be non-null, aligned, and point to a valid `DrmFramebuffer` with initialized `format` and plane `pitches`.
/// - `clip` must be non-null, aligned, and point to a valid `DrmRect` bounded by `fb`.
pub unsafe extern "C" fn drm_fb_memcpy(
    dst: *mut IosysMap,
    dst_pitch: *const u32,
    src: *const IosysMap,
    fb: *const DrmFramebuffer,
    clip: *const DrmRect,
) {
    // SAFETY: `fb` is non-null and points to a valid `DrmFramebuffer`; reading its format descriptor pointer.
    let format = unsafe { (*fb).format };
    // SAFETY: `clip` is a valid, aligned pointer to `DrmRect`; borrowing as shared reference to compute clip height.
    let lines = drm_rect_height(unsafe { &*clip }) as u32;

    // SAFETY: `format` is an initialized `DrmFormatInfo` struct; reading `num_planes` (at most `DRM_FORMAT_MAX_PLANES`).
    for i in 0..unsafe { (*format).num_planes as usize } {
        // SAFETY: Plane index `i` is within `0..num_planes`; reading bits-per-pixel for plane `i`.
        let bpp_i = unsafe { drm_format_info_bpp(format, i as i32) };
        let cpp_i = bpp_i.div_ceil(8);
        // SAFETY: `clip` is a valid pointer to `DrmRect`; borrowing as reference to compute clip width.
        let len_i = (drm_rect_width(unsafe { &*clip }) as u32)
            .wrapping_mul(bpp_i)
            .div_ceil(8) as usize;
        let mut dst_pitch_i = if dst_pitch.is_null() {
            0
        } else {
            // SAFETY: `dst_pitch` is non-null and plane index `i < num_planes` is within the supplied pitch array.
            unsafe { *dst_pitch.add(i) }
        };
        // SAFETY: `dst` points to an array of at least `num_planes` `IosysMap` descriptors; copying entry `i` by value.
        let mut dst_i: IosysMap = unsafe { ptr::read(dst.add(i)) };
        // SAFETY: `src` points to an array of at least `num_planes` `IosysMap` descriptors; copying entry `i` by value.
        let mut src_i: IosysMap = unsafe { ptr::read(src.add(i)) };

        if dst_pitch_i == 0 {
            dst_pitch_i = len_i as u32;
        }

        // SAFETY: Plane index `i` has stride `(*fb).pitches[i]` and `cpp_i` bytes per pixel; `clip_offset`
        // computes the initial byte offset for plane `i` within the source buffer.
        unsafe {
            iosys_map_incr(
                &mut src_i,
                clip_offset(&*clip, (*fb).pitches[i], cpp_i) as usize,
            );
        }
        for _ in 0..lines {
            // TODO (upstream): handle src_i in I/O memory here.
            // SAFETY: `src_i._anon_0.vaddr` provides `len_i` readable bytes for the current row;
            // `dst_i` provides `len_i` writable bytes at offset 0 in the destination mapping.
            unsafe { iosys_map_memcpy_to(&mut dst_i, 0, src_i._anon_0.vaddr, len_i) };
            // SAFETY: Advancing `src_i` by pitch `(*fb).pitches[i]` stays within the plane's allocated scanout buffer.
            unsafe { iosys_map_incr(&mut src_i, (*fb).pitches[i] as usize) };
            // SAFETY: Advancing `dst_i` by `dst_pitch_i` stays within the destination plane's allocated buffer.
            unsafe { iosys_map_incr(&mut dst_i, dst_pitch_i as usize) };
        }
    }
}
